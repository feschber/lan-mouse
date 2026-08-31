//! System tray (notification area) integration for Windows.
//!
//! Counterpart to `macos_status_item` / `linux_status_item`: registers a
//! tray icon with an "Open / Quit" menu so the main window can close
//! into the tray instead of quitting the application. Uses the
//! `tray-icon` crate (`Shell_NotifyIconW` under the hood), which also
//! re-adds the icon after an explorer.exe restart (TaskbarCreated
//! broadcast).
//!
//! Must be called on the GTK thread: the tray's hidden message window
//! is created on the calling thread and its WndProc is serviced by the
//! win32 message pump inside the GLib main loop.

use std::cell::RefCell;

use adw::prelude::*;
use gtk::{gdk_pixbuf, gio, glib, glib::clone};
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

use crate::window::Window;

const APP_ICON_RESOURCE: &str = "/de/feschber/LanMouse/icons/de.feschber.LanMouse.svg";

/// Raster fallback: PNG decoding is built into gdk-pixbuf, while SVG
/// needs the external librsvg loader module, which distributed builds
/// may lack.
const APP_ICON_RESOURCE_PNG: &str =
    "/de/feschber/LanMouse/icons/128x128/apps/de.feschber.LanMouse.png";

/// 32 px stays crisp at the common 100–200% DPI scales; Windows scales
/// down to the actual small-icon metric as needed.
const ICON_SIZE: i32 = 32;

enum TrayEvent {
    Open,
    Quit,
}

struct StatusItem {
    /// Keeps the application running while no window is visible.
    _hold: gio::ApplicationHoldGuard,
    /// Dropping the `TrayIcon` removes the icon from the notification
    /// area, so it lives here for the lifetime of the app.
    _tray_icon: TrayIcon,
}

thread_local! {
    static STATUS_ITEM: RefCell<Option<StatusItem>> = const { RefCell::new(None) };
}

fn forward(tx: &async_channel::Sender<TrayEvent>, event: TrayEvent) {
    // The tray handlers run inside the WndProc on the GTK thread — the
    // same thread that drains the channel — so blocking on a full
    // channel would deadlock. Dropping an event on overflow is fine:
    // both events are idempotent user actions.
    if tx.try_send(event).is_err() {
        log::warn!("tray event dropped: frontend channel closed or full");
    }
}

/// Set up the tray icon. Returns whether it was registered; on `false`
/// the caller must keep the default close-means-quit behavior, or the
/// application would become unreachable once the window is hidden.
pub fn setup(app: &adw::Application, window: &Window) -> bool {
    let already_initialized = STATUS_ITEM.with(|item| item.borrow().is_some());
    if already_initialized {
        return true;
    }

    let Some(icon) = render_icon_rgba(ICON_SIZE) else {
        log::warn!("failed to render tray icon, closing the window will quit");
        return false;
    };

    let menu = Menu::new();
    let open_item = MenuItem::new("Open Lan Mouse", true, None);
    let quit_item = MenuItem::new("Quit Lan Mouse", true, None);
    if let Err(e) = menu.append_items(&[
        &open_item as &dyn IsMenuItem,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ]) {
        log::warn!("failed to build tray menu ({e}), closing the window will quit");
        return false;
    }

    let (tx, rx) = async_channel::bounded(4);

    // The handlers must be installed before build(): events dispatch
    // from the tray's WndProc as soon as the icon exists. They require
    // `Send + Sync`, so they cannot capture GTK objects — forward
    // through the channel to the main-loop drain below instead.
    let open_id = open_item.id().clone();
    let quit_id = quit_item.id().clone();
    let menu_tx = tx.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if *event.id() == open_id {
            forward(&menu_tx, TrayEvent::Open);
        } else if *event.id() == quit_id {
            forward(&menu_tx, TrayEvent::Quit);
        }
    }));
    // Single left click opens the window — same as SNI `activate` on
    // Linux. DoubleClick is redundant with that (the first click
    // already opened) and is ignored.
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            forward(&tx, TrayEvent::Open);
        }
    }));

    let tray_icon = match TrayIconBuilder::new()
        .with_tooltip("Lan Mouse")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        // left click presents the window; the menu stays on right click
        .with_menu_on_left_click(false)
        .build()
    {
        Ok(tray_icon) => tray_icon,
        Err(e) => {
            log::warn!("no system tray available ({e}), closing the window will quit");
            MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
            TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
            return false;
        }
    };
    log::debug!("windows_status_item registered");

    STATUS_ITEM.with(|item| {
        item.replace(Some(StatusItem {
            _hold: app.hold(),
            _tray_icon: tray_icon,
        }));
    });

    glib::spawn_future_local(clone!(
        #[weak]
        app,
        #[weak]
        window,
        async move {
            while let Ok(event) = rx.recv().await {
                match event {
                    TrayEvent::Open => window.present(),
                    TrayEvent::Quit => app.quit(),
                }
            }
        }
    ));

    true
}

/// Render the bundled app icon into packed RGBA8 for
/// [`Icon::from_rgba`]: the SVG for crispness where the loader is
/// available, the pre-rendered PNG otherwise.
fn render_icon_rgba(size: i32) -> Option<Icon> {
    [APP_ICON_RESOURCE, APP_ICON_RESOURCE_PNG]
        .iter()
        .find_map(|resource| render_resource_rgba(resource, size))
}

fn render_resource_rgba(resource: &str, size: i32) -> Option<Icon> {
    let pixbuf = match gdk_pixbuf::Pixbuf::from_resource_at_scale(resource, size, size, true) {
        Ok(pixbuf) => pixbuf,
        Err(e) => {
            log::debug!("failed to render tray icon {resource} at {size}px: {e}");
            return None;
        }
    };

    let width = pixbuf.width();
    let height = pixbuf.height();
    let rowstride = pixbuf.rowstride() as usize;
    let n_channels = pixbuf.n_channels() as usize;
    if pixbuf.bits_per_sample() != 8 || !(3..=4).contains(&n_channels) {
        log::warn!("unexpected pixbuf format for tray icon");
        return None;
    }
    let bytes = pixbuf.read_pixel_bytes();

    // pixbuf rows are RGB(A) with rowstride padding; from_rgba wants
    // packed RGBA8.
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for row in 0..height as usize {
        let row = &bytes[row * rowstride..];
        for pixel in 0..width as usize {
            let pixel = &row[pixel * n_channels..][..n_channels];
            let alpha = if n_channels == 4 { pixel[3] } else { 0xff };
            data.extend_from_slice(&[pixel[0], pixel[1], pixel[2], alpha]);
        }
    }

    match Icon::from_rgba(data, width as u32, height as u32) {
        Ok(icon) => Some(icon),
        Err(e) => {
            log::warn!("failed to create tray icon: {e}");
            None
        }
    }
}
