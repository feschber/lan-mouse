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
/// can pick from. For each target size we render the SVG at 4× that
/// size, scan the actual non-transparent bounding box, and crop to
/// it (with a tiny edge margin so anti-aliased pixels aren't shaved
/// off). The host then scales the pixmap to its own `icon-size`;
/// because our pixmap is exactly the glyph with no surrounding
/// padding, the rendered icon fills the slot edge-to-edge — visibly
/// larger than neighbouring icons that include theme padding.
fn render_tray_pixmaps() -> Vec<Icon> {
    // Distinct from the desktop icon — see comments in
    // resources/icons/lan-mouse-tray.svg. Tight viewBox + simple
    // silhouette so the glyph remains readable at 12-22 px.
    const SVG_RESOURCE: &str = "/de/feschber/LanMouse/icons/lan-mouse-tray.svg";
    // High-density render so the alpha bbox scan operates on a
    // reasonably anti-aliased pixmap; the result is then cropped and
    // resampled by the host.
    const RENDER_OVERSAMPLE: i32 = 4;
    // Provide a few common host sizes; SNI hosts pick the closest fit
    // and downscale, which is much sharper than upscaling a single
    // tiny pixmap.
    const TARGET_SIZES: &[i32] = &[16, 22, 32, 48, 64];

    let mut icons = Vec::with_capacity(TARGET_SIZES.len());
    for &target in TARGET_SIZES {
        let render = target * RENDER_OVERSAMPLE;
        let Ok(pixbuf) = Pixbuf::from_resource_at_scale(SVG_RESOURCE, render, render, true) else {
            log::warn!("linux_tray: failed to render SVG at {render}px");
            continue;
        };
        let Some(icon) = pixbuf_bbox_crop_argb32(&pixbuf) else {
            continue;
        };
        log::debug!(
            "linux_tray: tray pixmap target={target} → cropped {}x{}",
            icon.width,
            icon.height
        );
        icons.push(icon);
    }
    if icons.is_empty() {
        log::warn!("linux_tray: no pixmaps rendered; tray will fall back to icon name");
    }
    icons
}

/// Scan the alpha channel for the bounding box of visible pixels,
/// crop the pixbuf to that bbox (as a square so the host doesn't
/// letterbox) and convert to ARGB32 in network byte order — the
/// encoding the StatusNotifierItem spec requires for `IconPixmap`.
///
/// Adds a 1-pixel transparent margin so soft-edged anti-aliased
/// pixels at the rim aren't clipped to a hard edge by aggressive
/// host downscaling.
fn pixbuf_bbox_crop_argb32(pixbuf: &Pixbuf) -> Option<Icon> {
    if pixbuf.n_channels() != 4 || pixbuf.bits_per_sample() != 8 {
        return None;
    }
    let w = pixbuf.width();
    let h = pixbuf.height();
    let rowstride = pixbuf.rowstride() as usize;
    let bytes = pixbuf.read_pixel_bytes();
    let pixels = bytes.as_ref();

    // Anything below this alpha is treated as background — guards
    // against a few stray sub-1% alpha pixels (Inkscape's renderer
    // sometimes emits them outside the visible glyph) anchoring the
    // bbox to the canvas edges.
    const ALPHA_THRESHOLD: u8 = 8;

    let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, -1i32, -1i32);
    for y in 0..h {
        for x in 0..w {
            let i = (y as usize) * rowstride + (x as usize) * 4;
            if pixels[i + 3] >= ALPHA_THRESHOLD {
                if x < min_x {
                    min_x = x;
                }
                if y < min_y {
                    min_y = y;
                }
                if x > max_x {
                    max_x = x;
                }
                if y > max_y {
                    max_y = y;
                }
            }
        }
    }
    if max_x < 0 || max_y < 0 {
        return None;
    }

    // 1-px breathing room on each side, then square up by padding
    // the shorter axis so the host doesn't letterbox.
    let pad = 1;
    let x0 = (min_x - pad).max(0);
    let y0 = (min_y - pad).max(0);
    let x1 = (max_x + pad + 1).min(w);
    let y1 = (max_y + pad + 1).min(h);
    let crop_w = x1 - x0;
    let crop_h = y1 - y0;
    let side = crop_w.max(crop_h);
    let off_x = (side - crop_w) / 2;
    let off_y = (side - crop_h) / 2;

    let mut data = vec![0u8; (side * side * 4) as usize];
    for sy in 0..crop_h {
        for sx in 0..crop_w {
            let src = ((y0 + sy) as usize) * rowstride + ((x0 + sx) as usize) * 4;
            let dst = (((off_y + sy) * side + (off_x + sx)) * 4) as usize;
            // gdk-pixbuf RGBA → SNI ARGB32 (network byte order).
            data[dst] = pixels[src + 3];
            data[dst + 1] = pixels[src];
            data[dst + 2] = pixels[src + 1];
            data[dst + 3] = pixels[src + 2];
        }
    }
    Some(Icon {
        width: side,
        height: side,
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
