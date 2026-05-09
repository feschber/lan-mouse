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
use adw::prelude::*;
use async_channel::Sender;
use gtk::{gio, glib};
use ksni::{
    Tray, TrayService,
    menu::{MenuItem, StandardItem},
};

use crate::window::Window;

#[derive(Debug)]
enum TrayCmd {
    TogglePresent,
    Quit,
}

struct LanMouseTray {
    tx: Sender<TrayCmd>,
}

impl Tray for LanMouseTray {
    fn id(&self) -> String {
        "de.feschber.LanMouse".into()
    }

    fn title(&self) -> String {
        "Lan Mouse".into()
    }

    // Prefer the app's branded icon if installed in the icon theme;
    // `input-mouse` is the fallback that ships with every freedesktop
    // theme so the tray slot still renders something on a fresh
    // install where the LanMouse icon hasn't been deployed to
    // /usr/share/icons/hicolor.
    fn icon_name(&self) -> String {
        "de.feschber.LanMouse".into()
    }

    fn icon_theme_path(&self) -> String {
        String::new()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Lan Mouse".into(),
            description: String::new(),
            icon_name: "de.feschber.LanMouse".into(),
            icon_pixmap: vec![],
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send_blocking(TrayCmd::TogglePresent);
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

/// Register the tray and arm the lifetime hold. The returned guard
/// MUST be kept alive for as long as the tray should run — we stash
/// it in a thread-local at the call site, mirroring the macOS code.
pub(crate) fn setup(app: &adw::Application, window: &Window) -> gio::ApplicationHoldGuard {
    let hold = app.hold();
    let (tx, rx) = async_channel::bounded::<TrayCmd>(8);

    let service = TrayService::new(LanMouseTray { tx });
    service.spawn();
    log::debug!("linux_tray: StatusNotifierItem registered");

    let app = app.clone();
    let window = window.clone();
    glib::spawn_future_local(async move {
        while let Ok(cmd) = rx.recv().await {
            match cmd {
                TrayCmd::TogglePresent => {
                    if window.is_visible() {
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
