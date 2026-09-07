//! StatusNotifierItem (system tray) integration for Linux.
//!
//! Counterpart to `macos_status_item`: registers a tray icon with an
//! "Open / Quit" menu so the main window can close into the tray
//! instead of quitting the application. Uses the StatusNotifierItem
//! D-Bus specification via `ksni` — desktops without an SNI host
//! (e.g. GNOME without the AppIndicator extension) are detected at
//! registration time and reported via [`setup`] returning `false`.

use std::cell::RefCell;

use adw::prelude::*;
use gtk::{gdk_pixbuf, gio, glib, glib::clone};
use ksni::blocking::TrayMethods;

use crate::window::Window;

/// Sizes (px) the tray icon pixmap is pre-rendered at. SNI hosts pick
/// the closest size; 24 covers common panels, 48 hidpi ones.
const ICON_SIZES: [i32; 2] = [24, 48];

const APP_ICON_RESOURCE: &str = "/de/feschber/LanMouse/icons/de.feschber.LanMouse.svg";

enum TrayEvent {
    Open,
    Quit,
}

struct StatusItem {
    /// Keeps the application running while no window is visible.
    _hold: gio::ApplicationHoldGuard,
    _handle: ksni::blocking::Handle<LanMouseTray>,
}

thread_local! {
    static STATUS_ITEM: RefCell<Option<StatusItem>> = const { RefCell::new(None) };
}

struct LanMouseTray {
    tx: async_channel::Sender<TrayEvent>,
    icons: Vec<ksni::Icon>,
}

impl LanMouseTray {
    fn send(&self, event: TrayEvent) {
        // ksni invokes menu callbacks on its own service thread; hand
        // the event to the GTK main loop instead of touching UI here.
        if self.tx.send_blocking(event).is_err() {
            log::warn!("tray event dropped: frontend channel closed");
        }
    }
}

impl ksni::Tray for LanMouseTray {
    fn id(&self) -> String {
        "de.feschber.LanMouse".into()
    }

    fn title(&self) -> String {
        "Lan Mouse".into()
    }

    fn icon_name(&self) -> String {
        // resolved from the icon theme where the app is installed
        "de.feschber.LanMouse".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        // fallback for setups without the themed icon (e.g. AppImage)
        self.icons.clone()
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        vec![
            StandardItem {
                label: "Open Lan Mouse".into(),
                activate: Box::new(|tray: &mut Self| tray.send(TrayEvent::Open)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit Lan Mouse".into(),
                activate: Box::new(|tray: &mut Self| tray.send(TrayEvent::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayEvent::Open);
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

    let (tx, rx) = async_channel::bounded(4);
    let tray = LanMouseTray {
        tx,
        icons: render_icon_pixmaps(),
    };

    let handle = match tray.spawn() {
        Ok(handle) => handle,
        Err(e) => {
            log::warn!("no system tray available ({e}), closing the window will quit");
            return false;
        }
    };
    log::debug!("linux_status_item registered");

    STATUS_ITEM.with(|item| {
        item.replace(Some(StatusItem {
            _hold: app.hold(),
            _handle: handle,
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

fn render_icon_pixmaps() -> Vec<ksni::Icon> {
    ICON_SIZES
        .iter()
        .filter_map(|&size| render_icon_pixmap(size))
        .collect()
}

fn render_icon_pixmap(size: i32) -> Option<ksni::Icon> {
    let pixbuf =
        match gdk_pixbuf::Pixbuf::from_resource_at_scale(APP_ICON_RESOURCE, size, size, true) {
            Ok(pixbuf) => pixbuf,
            Err(e) => {
                log::warn!("failed to render tray icon at {size}px: {e}");
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

    // pixbuf rows are RGB(A) with padding; SNI wants packed ARGB32 in
    // network byte order.
    let mut data = Vec::with_capacity(width as usize * height as usize * 4);
    for row in 0..height as usize {
        let row = &bytes[row * rowstride..];
        for pixel in 0..width as usize {
            let pixel = &row[pixel * n_channels..][..n_channels];
            let alpha = if n_channels == 4 { pixel[3] } else { 0xff };
            data.extend_from_slice(&[alpha, pixel[0], pixel[1], pixel[2]]);
        }
    }

    Some(ksni::Icon {
        width,
        height,
        data,
    })
}
