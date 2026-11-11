//! Tray icon for the service on Linux and the BSDs (StatusNotifierItem over
//! D-Bus, shown by Plasma and by the XFCE panel's Status Tray plugin, among
//! others).
//!
//! The menu starts and stops sharing, opens the window (GTK builds) and quits
//! the service. The icon belongs to the service, so it is there whether the
//! service was started by the window or on its own (`lan-mouse daemon`).
//! Setting `LAN_MOUSE_NO_TRAY` disables it.

use ksni::{MenuItem, ToolTip, Tray, TrayMethods, menu::StandardItem};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// Requests from the tray menu to the service.
#[derive(Debug)]
pub(crate) enum TrayCommand {
    /// start (`true`) or stop (`false`) sharing
    SetSharing(bool),
    /// shut the service down
    Quit,
}

struct LanMouseTray {
    /// whether any device is active (as last reported by the service)
    sharing: bool,
    tx: UnboundedSender<TrayCommand>,
}

impl Tray for LanMouseTray {
    fn id(&self) -> String {
        "lan-mouse".into()
    }

    fn title(&self) -> String {
        "Lan Mouse".into()
    }

    fn icon_name(&self) -> String {
        "input-mouse".into()
    }

    fn tool_tip(&self) -> ToolTip {
        let state = if self.sharing {
            "sharing is on"
        } else {
            "sharing is off"
        };
        ToolTip {
            title: "Lan Mouse".into(),
            description: state.into(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        #[cfg(feature = "gtk")]
        open_window();
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = Vec::new();
        #[cfg(feature = "gtk")]
        items.push(
            StandardItem {
                label: "Open Lan Mouse".into(),
                icon_name: "input-mouse".into(),
                activate: Box::new(|_: &mut Self| open_window()),
                ..Default::default()
            }
            .into(),
        );
        let (label, icon_name) = if self.sharing {
            ("Stop sharing", "media-playback-stop")
        } else {
            ("Start sharing", "media-playback-start")
        };
        items.push(
            StandardItem {
                label: label.into(),
                icon_name: icon_name.into(),
                activate: Box::new(|tray: &mut Self| {
                    let start = !tray.sharing;
                    tray.sharing = start;
                    let _ = tray.tx.send(TrayCommand::SetSharing(start));
                }),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit Lan Mouse".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.tx.send(TrayCommand::Quit);
                }),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// Starts the window. It attaches to the running service; if a window is
/// already open, GTK hands the request to it and it comes to the front.
#[cfg(feature = "gtk")]
fn open_window() {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return log::warn!("tray: cannot find the lan-mouse executable: {e}"),
    };
    let mut cmd = std::process::Command::new(exe);
    // the window should show even if the service was started hidden
    cmd.env_remove("LAN_MOUSE_HIDDEN");
    // lets the window come to the front on Wayland
    if let Some(token) = ksni::current_xdg_activation_token() {
        cmd.env("XDG_ACTIVATION_TOKEN", token);
    }
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => log::warn!("tray: could not start the window: {e}"),
    }
}

/// The tray as seen by the service.
#[derive(Default)]
pub(crate) struct ServiceTray {
    handle: Option<ksni::Handle<LanMouseTray>>,
    rx: Option<UnboundedReceiver<TrayCommand>>,
    /// sharing state last shown, `None` forces the next update
    shown_sharing: Option<bool>,
}

impl ServiceTray {
    /// Shows the tray icon. Without a session bus, or with `LAN_MOUSE_NO_TRAY`
    /// set, the service runs without one.
    pub(crate) async fn new(sharing: bool) -> Self {
        if std::env::var_os("LAN_MOUSE_NO_TRAY").is_some() {
            log::info!("tray icon disabled (LAN_MOUSE_NO_TRAY is set)");
            return Self::default();
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let tray = LanMouseTray { sharing, tx };
        // when started at login the panel may not be up yet; the icon
        // appears once it is
        match tray.assume_sni_available(true).spawn().await {
            Ok(handle) => {
                log::info!("tray icon started");
                Self {
                    handle: Some(handle),
                    rx: Some(rx),
                    shown_sharing: Some(sharing),
                }
            }
            Err(e) => {
                log::warn!("running without a tray icon: {e}");
                Self::default()
            }
        }
    }

    /// The next request from the tray menu. Never completes without a tray.
    pub(crate) async fn command(&mut self) -> TrayCommand {
        match self.rx.as_mut() {
            Some(rx) => match rx.recv().await {
                Some(cmd) => cmd,
                None => std::future::pending().await,
            },
            None => std::future::pending().await,
        }
    }

    /// Makes the next [`Self::set_sharing`] update the icon even if the state
    /// did not change (the menu already flipped its own copy).
    pub(crate) fn invalidate(&mut self) {
        self.shown_sharing = None;
    }

    /// Shows whether sharing is on.
    pub(crate) async fn set_sharing(&mut self, sharing: bool) {
        if self.shown_sharing == Some(sharing) {
            return;
        }
        self.shown_sharing = Some(sharing);
        if let Some(handle) = &self.handle {
            handle.update(|tray| tray.sharing = sharing).await;
        }
    }

    /// Removes the icon.
    pub(crate) async fn shutdown(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown().await;
        }
    }
}
