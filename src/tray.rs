//! Tray icon for the service on Linux and the BSDs (StatusNotifierItem over
//! D-Bus, shown by Plasma and by the XFCE panel's Status Tray plugin, among
//! others).
//!
//! The menu starts and stops sharing, opens the window (GTK builds) and quits
//! the service. The icon belongs to the service, so it is there whether the
//! service was started by the window or on its own (`lan-mouse daemon`).
//! The icon is blue while this device shares its mouse and keyboard, grey with
//! a blue dot while it does not but another device is connected to it (so a
//! device that only receives shows that too), and grey otherwise.
//! Setting `LAN_MOUSE_NO_TRAY` disables it.

use std::{
    collections::{BTreeSet, HashSet},
    net::{IpAddr, SocketAddr},
};

use ksni::{Icon, MenuItem, ToolTip, Tray, TrayMethods, menu::StandardItem};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// Requests from the tray menu to the service.
#[derive(Debug)]
pub(crate) enum TrayCommand {
    /// start (`true`) or stop (`false`) sharing
    SetSharing(bool),
    /// shut the service down
    Quit,
}

/// What the icon, tooltip and menu show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TrayState {
    /// number of devices this one shares its mouse and keyboard with
    sharing: usize,
    /// devices connected to this one
    connected_from: BTreeSet<IpAddr>,
}

impl TrayState {
    fn new(sharing: usize, connected_from: &HashSet<SocketAddr>) -> Self {
        Self {
            sharing,
            connected_from: connected_from
                .iter()
                .map(|addr| addr.ip().to_canonical())
                .collect(),
        }
    }

    fn look(&self) -> IconLook {
        if self.sharing > 0 {
            IconLook::Sharing
        } else if !self.connected_from.is_empty() {
            IconLook::Connected
        } else {
            IconLook::Idle
        }
    }
}

/// How the icon looks. Sharing wins over connected: once a device has been
/// handed the mouse, it usually keeps a connection back open (to hand it back
/// at its edge), so a lit icon for "connected" would never go out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IconLook {
    /// this device shares its mouse and keyboard: blue
    Sharing,
    /// not sharing, but another device is connected to this one: grey with a
    /// blue dot
    Connected,
    /// neither: grey
    Idle,
}

struct LanMouseTray {
    state: TrayState,
    /// the icon for each look, in each size (drawn once)
    sharing_icon: Vec<Icon>,
    connected_icon: Vec<Icon>,
    idle_icon: Vec<Icon>,
    tx: UnboundedSender<TrayCommand>,
}

impl Tray for LanMouseTray {
    fn id(&self) -> String {
        "lan-mouse".into()
    }

    fn title(&self) -> String {
        "Lan Mouse".into()
    }

    // no `icon_name`: tray hosts prefer a theme icon over the pixmap, and no
    // theme has these looks
    fn icon_pixmap(&self) -> Vec<Icon> {
        match self.state.look() {
            IconLook::Sharing => self.sharing_icon.clone(),
            IconLook::Connected => self.connected_icon.clone(),
            IconLook::Idle => self.idle_icon.clone(),
        }
    }

    fn tool_tip(&self) -> ToolTip {
        let mut lines = Vec::new();
        match self.state.sharing {
            0 => {}
            1 => lines.push("Sharing with 1 device".to_string()),
            n => lines.push(format!("Sharing with {n} devices")),
        }
        if !self.state.connected_from.is_empty() {
            let from: Vec<String> = self
                .state
                .connected_from
                .iter()
                .map(|ip| ip.to_string())
                .collect();
            lines.push(format!("Connected from {}", from.join(", ")));
        }
        if lines.is_empty() {
            lines.push("Sharing is off, no device connected".to_string());
        }
        ToolTip {
            title: "Lan Mouse".into(),
            description: lines.join("\n"),
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
        let (label, icon_name) = if self.state.sharing > 0 {
            ("Stop sharing", "media-playback-stop")
        } else {
            ("Start sharing", "media-playback-start")
        };
        items.push(
            StandardItem {
                label: label.into(),
                icon_name: icon_name.into(),
                // the service sends the new state back once it has changed
                activate: Box::new(|tray: &mut Self| {
                    let start = tray.state.sharing == 0;
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

/// Sizes the icon is drawn in; the tray host picks the one that fits.
const ICON_SIZES: [i32; 6] = [16, 22, 24, 32, 48, 64];

/// Colour (RGB) of the sharing mouse and of the connected dot.
const BLUE: [u8; 3] = [0x3d, 0xae, 0xe9];

/// Colour (RGB) and opacity of the grey mouse. Grey at 70% stays visible on
/// light and dark panels.
const GREY: [u8; 3] = [0x8c, 0x8c, 0x8c];
const GREY_OPACITY: f32 = 0.7;

fn icons(look: IconLook) -> Vec<Icon> {
    ICON_SIZES
        .iter()
        .map(|&size| draw_icon(size, look))
        .collect()
}

/// Draws a mouse (a rounded body with the buttons cut out), plus the dot at
/// the bottom right for [`IconLook::Connected`], as a `size` x `size`
/// StatusNotifierItem pixmap: ARGB32 (not premultiplied), network byte order.
/// Edges are anti-aliased from the distance of each pixel centre to them.
fn draw_icon(size: i32, look: IconLook) -> Icon {
    let (mouse_rgb, mouse_opacity) = match look {
        IconLook::Sharing => (BLUE, 1.0),
        IconLook::Connected | IconLook::Idle => (GREY, GREY_OPACITY),
    };
    let s = size as f32;
    // body: centre, half width, half height and corner radius (all in pixels)
    let (cx, cy) = (0.5 * s, 0.5 * s);
    let (half_w, half_h, radius) = (0.29 * s, 0.43 * s, 0.27 * s);
    // the line under the buttons, and half the width of the cuts
    let split_y = 0.40 * s;
    let half_cut = (0.035 * s).max(0.6);
    // the dot: centre and radius
    let (dot_x, dot_y, dot_r) = (0.78 * s, 0.78 * s, 0.18 * s);
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // distance to the body's edge (negative inside)
            let qx = (px - cx).abs() - (half_w - radius);
            let qy = (py - cy).abs() - (half_h - radius);
            let body = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius;
            // distance to the cuts: across the body under the buttons, and
            // down the middle between them
            let across = (py - split_y).abs();
            let between = if py < split_y {
                (px - cx).abs()
            } else {
                (px - cx).hypot(py - split_y)
            };
            let cut = across.min(between) - half_cut;
            let mut mouse =
                (0.5 - body).clamp(0.0, 1.0) * (cut + 0.5).clamp(0.0, 1.0) * mouse_opacity;
            let mut dot = 0.0;
            if look == IconLook::Connected {
                let to_dot = (px - dot_x).hypot(py - dot_y) - dot_r;
                dot = (0.5 - to_dot).clamp(0.0, 1.0);
                // a gap as wide as the cuts between the dot and the mouse
                mouse *= (to_dot - 2.0 * half_cut + 0.5).clamp(0.0, 1.0);
            }
            // the dot over the mouse
            let alpha = dot + mouse * (1.0 - dot);
            let mut rgb = mouse_rgb;
            if alpha > 0.0 {
                for (c, out) in rgb.iter_mut().enumerate() {
                    let mixed = BLUE[c] as f32 * dot + mouse_rgb[c] as f32 * mouse * (1.0 - dot);
                    *out = (mixed / alpha).round() as u8;
                }
            }
            let alpha = (alpha * 255.0).round() as u8;
            data.extend_from_slice(&[alpha, rgb[0], rgb[1], rgb[2]]);
        }
    }
    Icon {
        width: size,
        height: size,
        data,
    }
}

/// The tray as seen by the service.
#[derive(Default)]
pub(crate) struct ServiceTray {
    handle: Option<ksni::Handle<LanMouseTray>>,
    rx: Option<UnboundedReceiver<TrayCommand>>,
    /// what was last shown, to skip updates that change nothing
    shown_sharing: usize,
    shown_connected_from: HashSet<SocketAddr>,
}

impl ServiceTray {
    /// Shows the tray icon. Without a session bus, or with `LAN_MOUSE_NO_TRAY`
    /// set, the service runs without one.
    pub(crate) async fn new(sharing: usize, connected_from: &HashSet<SocketAddr>) -> Self {
        if std::env::var_os("LAN_MOUSE_NO_TRAY").is_some() {
            log::info!("tray icon disabled (LAN_MOUSE_NO_TRAY is set)");
            return Self::default();
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let tray = LanMouseTray {
            state: TrayState::new(sharing, connected_from),
            sharing_icon: icons(IconLook::Sharing),
            connected_icon: icons(IconLook::Connected),
            idle_icon: icons(IconLook::Idle),
            tx,
        };
        // when started at login the panel may not be up yet; the icon
        // appears once it is
        match tray.assume_sni_available(true).spawn().await {
            Ok(handle) => {
                log::info!("tray icon started");
                Self {
                    handle: Some(handle),
                    rx: Some(rx),
                    shown_sharing: sharing,
                    shown_connected_from: connected_from.clone(),
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

    /// Shows how many devices this one shares with and which devices are
    /// connected to it. Called after every event, so it returns early if
    /// nothing changed.
    pub(crate) async fn update(&mut self, sharing: usize, connected_from: &HashSet<SocketAddr>) {
        if sharing == self.shown_sharing && *connected_from == self.shown_connected_from {
            return;
        }
        self.shown_sharing = sharing;
        self.shown_connected_from = connected_from.clone();
        if let Some(handle) = &self.handle {
            let state = TrayState::new(sharing, connected_from);
            handle.update(|tray| tray.state = state).await;
        }
    }

    /// Removes the icon.
    pub(crate) async fn shutdown(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ARGB of the pixel at (x, y)
    fn pixel(icon: &Icon, x: i32, y: i32) -> [u8; 4] {
        let i = ((y * icon.width + x) * 4) as usize;
        icon.data[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn icon_sizes_and_shape() {
        for icon in icons(IconLook::Sharing) {
            let size = icon.width;
            assert_eq!(icon.height, size);
            assert_eq!(icon.data.len(), (size * size * 4) as usize);
            // body solid blue, corners and the cut between the buttons empty
            let [a, r, g, b] = pixel(&icon, size / 2, size * 3 / 4);
            assert_eq!((a, [r, g, b]), (255, BLUE));
            assert_eq!(pixel(&icon, 0, 0)[0], 0);
            assert_eq!(pixel(&icon, size - 1, size - 1)[0], 0);
            assert!(pixel(&icon, size / 2, size / 5)[0] < 128);
        }
    }

    #[test]
    fn grey_icons_are_fainter() {
        let sharing = draw_icon(22, IconLook::Sharing);
        for look in [IconLook::Connected, IconLook::Idle] {
            let [a, r, g, b] = pixel(&draw_icon(22, look), 11, 14);
            assert!(a < pixel(&sharing, 11, 14)[0]);
            assert_eq!([r, g, b], GREY);
        }
    }

    #[test]
    fn dot_only_when_connected() {
        for size in ICON_SIZES {
            // centre of the dot
            let at = size * 78 / 100;
            let [a, r, g, b] = pixel(&draw_icon(size, IconLook::Connected), at, at);
            assert_eq!((a, [r, g, b]), (255, BLUE));
            assert!(pixel(&draw_icon(size, IconLook::Idle), at, at)[0] < 128);
        }
    }

    #[test]
    fn look_follows_state() {
        let peer: SocketAddr = "192.168.1.90:4242".parse().unwrap();
        let connected = HashSet::from([peer]);
        let none = HashSet::new();
        assert_eq!(TrayState::new(0, &none).look(), IconLook::Idle);
        assert_eq!(TrayState::new(0, &connected).look(), IconLook::Connected);
        assert_eq!(TrayState::new(1, &none).look(), IconLook::Sharing);
        // a device handed the mouse keeps a connection back open
        assert_eq!(TrayState::new(1, &connected).look(), IconLook::Sharing);
    }
}
