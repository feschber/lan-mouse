//! Linux system tray (StatusNotifierItem) — keeps the daemon visible
//! and reachable when the GUI is hidden.
//!
//! Mirrors the role of [`crate::macos_status_item`] on macOS: holds an
//! [`gio::ApplicationHoldGuard`] so the GtkApplication survives the
//! main window being closed, exposes Open / Quit menu actions, and
//! toggles the window on left-click.
//!
//! Communication: ksni runs its own thread (spawned via
//! `TrayService::spawn`). Menu callbacks fire on that thread, so they
//! push commands through an `async_channel` consumed by a
//! `glib::spawn_future_local` task on the GTK main loop.
use std::time::{Duration, Instant};

use adw::prelude::*;
use async_channel::Sender;
use gtk::{gdk_pixbuf::Pixbuf, gio, glib};
use ksni::{
    Icon, Tray, TrayService,
    menu::{MenuItem, StandardItem},
};

use crate::window::Window;

#[derive(Debug)]
enum TrayCmd {
    TogglePresent,
    Quit,
}

/// waybar (and a few other SNI hosts) can fire two `Activate`
/// signals for a single physical click — so without this, every
/// click toggles twice and the window briefly flashes back to its
/// previous state. 300 ms is comfortably below human double-click
/// cadence while still swallowing the duplicates we see in practice.
const ACTIVATE_DEBOUNCE: Duration = Duration::from_millis(300);

struct LanMouseTray {
    tx: Sender<TrayCmd>,
    last_toggle: Option<Instant>,
    icon_pixmaps: Vec<Icon>,
}

impl LanMouseTray {
    fn try_emit_toggle(&mut self, source: &str) {
        let now = Instant::now();
        if let Some(prev) = self.last_toggle {
            if now.duration_since(prev) < ACTIVATE_DEBOUNCE {
                log::info!("linux_tray: dropped duplicate {source} within debounce window");
                return;
            }
        }
        self.last_toggle = Some(now);
        let _ = self.tx.send_blocking(TrayCmd::TogglePresent);
    }
}

impl Tray for LanMouseTray {
    fn id(&self) -> String {
        "de.feschber.LanMouse".into()
    }

    fn title(&self) -> String {
        "Lan Mouse".into()
    }

    // The icon name is still advertised so accessibility tools can
    // identify the item, but the actual rendering happens via
    // `icon_pixmap` below — we ship our own ARGB32 pixels so the icon
    // doesn't depend on the icon theme being installed and so the
    // glyph fills more of the host's tray slot than typical theme
    // icons (which carry their own internal padding).
    fn icon_name(&self) -> String {
        "de.feschber.LanMouse".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icon_pixmaps.clone()
    }

    fn icon_theme_path(&self) -> String {
        String::new()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Lan Mouse".into(),
            description: String::new(),
            icon_name: String::new(),
            icon_pixmap: self.icon_pixmaps.clone(),
        }
    }

    fn activate(&mut self, x: i32, y: i32) {
        log::info!("linux_tray: tray Activate at ({x},{y}) — toggling window");
        self.try_emit_toggle("Activate");
    }

    // Some tray hosts (older waybar, certain Plasma versions) treat
    // middle-click as the only "primary" interaction. Map it to the
    // same toggle so the icon is responsive regardless of which
    // event the host emits.
    fn secondary_activate(&mut self, x: i32, y: i32) {
        log::info!("linux_tray: tray SecondaryActivate at ({x},{y}) — toggling window");
        self.try_emit_toggle("SecondaryActivate");
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open Lan Mouse".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send_blocking(TrayCmd::TogglePresent);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit Lan Mouse".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send_blocking(TrayCmd::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Render the bundled SVG into a set of ARGB32 pixmaps the tray host
/// can pick from. We render at 1.3× the target size and crop the
/// center, so the visible glyph fills the entire pixmap with no
/// surrounding margin. The host then scales the pixmap to its own
/// `icon-size`; because our pixmap has no padding, the rendered icon
/// fills the slot more completely than typical theme icons (which
/// reserve ~10% padding by convention) — that's the "zoom" the user
/// asked for, achieved without changing the host's icon-size.
fn render_tray_pixmaps() -> Vec<Icon> {
    const SVG_RESOURCE: &str = "/de/feschber/LanMouse/icons/de.feschber.LanMouse.svg";
    // Zoom factor: how much of the SVG canvas we crop away to remove
    // the inherent padding. 1.3 trims ~23% of each edge, which lines
    // up well with how Inkscape-authored icons are typically padded.
    const ZOOM: f64 = 1.3;
    // Provide a few common host sizes; SNI hosts pick the closest fit
    // and downscale, which is much sharper than upscaling a single
    // tiny pixmap.
    const TARGET_SIZES: &[i32] = &[16, 22, 32, 48, 64];

    let mut icons = Vec::with_capacity(TARGET_SIZES.len());
    for &target in TARGET_SIZES {
        let render = (f64::from(target) * ZOOM).round() as i32;
        let Ok(pixbuf) = Pixbuf::from_resource_at_scale(SVG_RESOURCE, render, render, true) else {
            log::warn!("linux_tray: failed to render SVG at {render}px");
            continue;
        };
        let Some(icon) = pixbuf_center_crop_argb32(&pixbuf, target) else {
            continue;
        };
        icons.push(icon);
    }
    if icons.is_empty() {
        log::warn!("linux_tray: no pixmaps rendered; tray will fall back to icon name");
    }
    icons
}

/// Take the centre `target × target` region of an oversized RGBA
/// pixbuf and convert to ARGB32 (network byte order), the encoding
/// the StatusNotifierItem spec requires for `IconPixmap`.
fn pixbuf_center_crop_argb32(pixbuf: &Pixbuf, target: i32) -> Option<Icon> {
    if pixbuf.n_channels() != 4 || pixbuf.bits_per_sample() != 8 {
        return None;
    }
    let src_w = pixbuf.width();
    let src_h = pixbuf.height();
    if target <= 0 || target > src_w.min(src_h) {
        return None;
    }
    let x0 = (src_w - target) / 2;
    let y0 = (src_h - target) / 2;
    let rowstride = pixbuf.rowstride() as usize;
    let bytes = pixbuf.read_pixel_bytes();
    let pixels = bytes.as_ref();

    let mut data = Vec::with_capacity((target * target * 4) as usize);
    for y in y0..(y0 + target) {
        for x in x0..(x0 + target) {
            let i = (y as usize) * rowstride + (x as usize) * 4;
            // gdk-pixbuf encodes RGBA in memory order; SNI wants ARGB
            // in network byte order.
            data.push(pixels[i + 3]);
            data.push(pixels[i]);
            data.push(pixels[i + 1]);
            data.push(pixels[i + 2]);
        }
    }
    Some(Icon {
        width: target,
        height: target,
        data,
    })
}

/// Register the tray and arm the lifetime hold. The returned guard
/// MUST be kept alive for as long as the tray should run — we stash
/// it in a thread-local at the call site, mirroring the macOS code.
pub(crate) fn setup(app: &adw::Application, window: &Window) -> gio::ApplicationHoldGuard {
    let hold = app.hold();
    let (tx, rx) = async_channel::bounded::<TrayCmd>(8);
    let icon_pixmaps = render_tray_pixmaps();

    let service = TrayService::new(LanMouseTray {
        tx,
        last_toggle: None,
        icon_pixmaps,
    });
    service.spawn();
    log::debug!("linux_tray: StatusNotifierItem registered");

    let app = app.clone();
    let window = window.clone();
    glib::spawn_future_local(async move {
        while let Ok(cmd) = rx.recv().await {
            match cmd {
                TrayCmd::TogglePresent => {
                    let visible = window.is_visible();
                    log::info!(
                        "linux_tray: TogglePresent — currently visible={visible}, will {}",
                        if visible { "hide" } else { "present" }
                    );
                    if visible {
                        window.set_visible(false);
                    } else {
                        window.present();
                    }
                }
                TrayCmd::Quit => {
                    log::debug!("linux_tray: quit requested via tray menu");
                    app.quit();
                }
            }
        }
    });

    hold
}
