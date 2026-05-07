use std::{
    collections::{HashMap, HashSet},
    env::VarError,
    fmt::Display,
    io,
    net::{IpAddr, SocketAddr},
    str::FromStr,
};
use thiserror::Error;

#[cfg(unix)]
use std::{
    env,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

mod connect;
mod connect_async;
mod listen;

pub use connect::{FrontendEventReader, FrontendRequestWriter, connect};
pub use connect_async::{AsyncFrontendEventReader, AsyncFrontendRequestWriter, connect_async};
pub use listen::AsyncFrontendListener;

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error(transparent)]
    SocketPath(#[from] SocketPathError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("connection timed out")]
    Timeout,
}

#[derive(Debug, Error)]
pub enum IpcListenerCreationError {
    #[error("could not determine socket-path: `{0}`")]
    SocketPath(#[from] SocketPathError),
    #[error("service already running!")]
    AlreadyRunning,
    #[error("failed to bind lan-mouse socket: `{0}`")]
    Bind(io::Error),
}

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("io error occured: `{0}`")]
    Io(#[from] io::Error),
    #[error("invalid json: `{0}`")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    Listen(#[from] IpcListenerCreationError),
}

pub const DEFAULT_PORT: u16 = 4242;

/// Cross-platform identifier for an application whose clipboard
/// should not be shared with peers. The variant captures the
/// platform-specific string the OS surfaces for "this is app X":
/// macOS bundle ID, Windows executable basename, X11 `WM_CLASS`,
/// Wayland `xdg-toplevel.app_id`. Comparison is case-insensitive
/// within the same variant; cross-variant comparisons are always
/// `false` so a `LinuxX11("Chromium")` entry does not unexpectedly
/// suppress a Mac peer's `MacBundle("org.chromium.Chromium")`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AppIdent {
    /// macOS bundle identifier, e.g. `com.1password.1password7`.
    MacBundle(String),
    /// Windows executable basename (lowercased), e.g.
    /// `1password.exe`.
    WindowsExe(String),
    /// X11 `WM_CLASS` instance/name (lowercased), e.g. `firefox`.
    LinuxX11(String),
    /// Wayland `xdg-toplevel.app_id` (lowercased), e.g.
    /// `org.mozilla.firefox`.
    LinuxWayland(String),
}

impl AppIdent {
    /// Case-insensitive equality within the same variant.
    pub fn matches(&self, other: &AppIdent) -> bool {
        match (self, other) {
            (AppIdent::MacBundle(a), AppIdent::MacBundle(b))
            | (AppIdent::WindowsExe(a), AppIdent::WindowsExe(b))
            | (AppIdent::LinuxX11(a), AppIdent::LinuxX11(b))
            | (AppIdent::LinuxWayland(a), AppIdent::LinuxWayland(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }

    /// Human-readable rendering: `value (kind)` so the GUI can show
    /// `1password.exe (Windows)` rather than the raw enum.
    pub fn label(&self) -> String {
        match self {
            AppIdent::MacBundle(v) => format!("{v} (macOS bundle)"),
            AppIdent::WindowsExe(v) => format!("{v} (Windows)"),
            AppIdent::LinuxX11(v) => format!("{v} (X11)"),
            AppIdent::LinuxWayland(v) => format!("{v} (Wayland)"),
        }
    }
}

/// Per-OS clipboard suppression lists. Each host machine reads and
/// writes only the field that matches its own OS (`host()` /
/// `host_mut()`); the remaining fields round-trip through
/// `config.toml` untouched, so a single config file shipped across
/// machines (dotfiles, Syncthing, …) keeps each machine's
/// suppressed-app list intact. Strings are opaque platform
/// identifiers in the section's natural shape — bundle ID for
/// macOS, exe basename for Windows, etc.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardSuppression {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub macos: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linux_wayland: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linux_x11: Vec<String>,
}

impl ClipboardSuppression {
    /// Borrow the host machine's slot — the only one the GUI/CLI on
    /// this device ever interacts with. On Linux the choice between
    /// `linux_wayland` and `linux_x11` is decided at runtime via
    /// `WAYLAND_DISPLAY` so a single binary serves both session
    /// types correctly.
    pub fn host(&self) -> &Vec<String> {
        match HostKind::current() {
            HostKind::MacBundle => &self.macos,
            HostKind::WindowsExe => &self.windows,
            HostKind::LinuxWayland => &self.linux_wayland,
            HostKind::LinuxX11 => &self.linux_x11,
        }
    }

    /// Mutable borrow of [`Self::host`] for add/remove operations.
    pub fn host_mut(&mut self) -> &mut Vec<String> {
        match HostKind::current() {
            HostKind::MacBundle => &mut self.macos,
            HostKind::WindowsExe => &mut self.windows,
            HostKind::LinuxWayland => &mut self.linux_wayland,
            HostKind::LinuxX11 => &mut self.linux_x11,
        }
    }

    /// True when every per-OS slot is empty — used by the config
    /// writer to drop the key entirely instead of writing an empty
    /// table on save.
    pub fn is_empty(&self) -> bool {
        self.macos.is_empty()
            && self.windows.is_empty()
            && self.linux_wayland.is_empty()
            && self.linux_x11.is_empty()
    }
}

/// The [`AppIdent`] variant that this binary's host OS produces.
/// Computed once per call (cheap) so a Wayland session that
/// starts/stops mid-process picks up the change at the next
/// suppression check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    MacBundle,
    WindowsExe,
    LinuxWayland,
    LinuxX11,
}

impl HostKind {
    pub fn current() -> Self {
        #[cfg(target_os = "macos")]
        {
            HostKind::MacBundle
        }
        #[cfg(windows)]
        {
            HostKind::WindowsExe
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            if std::env::var_os("WAYLAND_DISPLAY")
                .map(|v| !v.is_empty())
                .unwrap_or(false)
            {
                HostKind::LinuxWayland
            } else {
                HostKind::LinuxX11
            }
        }
    }

    /// Wrap a host-OS identifier string in the matching [`AppIdent`]
    /// variant for the runtime suppression check.
    pub fn make_ident(self, value: String) -> AppIdent {
        match self {
            HostKind::MacBundle => AppIdent::MacBundle(value),
            HostKind::WindowsExe => AppIdent::WindowsExe(value),
            HostKind::LinuxWayland => AppIdent::LinuxWayland(value),
            HostKind::LinuxX11 => AppIdent::LinuxX11(value),
        }
    }

    /// Short noun used in GUI prompts ("Bundle ID", "Executable
    /// name", "WM_CLASS", …).
    pub fn entry_noun(self) -> &'static str {
        match self {
            HostKind::MacBundle => "Bundle ID",
            HostKind::WindowsExe => "Executable name",
            HostKind::LinuxWayland => "App ID",
            HostKind::LinuxX11 => "WM_CLASS",
        }
    }

    /// Placeholder shown inside the GUI's add-an-app text field.
    pub fn placeholder(self) -> &'static str {
        match self {
            HostKind::MacBundle => "e.g. com.1password.1password7",
            HostKind::WindowsExe => "e.g. 1Password.exe",
            HostKind::LinuxWayland => "e.g. org.keepassxc.KeePassXC",
            HostKind::LinuxX11 => "e.g. KeePassXC",
        }
    }
}

/// Convenience: wrap `value` in the host-OS [`AppIdent`] variant.
/// Equivalent to `HostKind::current().make_ident(value)`.
pub fn host_app_ident(value: String) -> AppIdent {
    HostKind::current().make_ident(value)
}

#[derive(Debug, Default, Eq, Hash, PartialEq, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Position {
    #[default]
    Left,
    Right,
    Top,
    Bottom,
}

impl Position {
    pub fn opposite(&self) -> Self {
        match self {
            Position::Left => Position::Right,
            Position::Right => Position::Left,
            Position::Top => Position::Bottom,
            Position::Bottom => Position::Top,
        }
    }
}

#[derive(Debug, Error)]
#[error("not a valid position: {pos}")]
pub struct PositionParseError {
    pos: String,
}

impl FromStr for Position {
    type Err = PositionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "top" => Ok(Self::Top),
            "bottom" => Ok(Self::Bottom),
            _ => Err(PositionParseError { pos: s.into() }),
        }
    }
}

impl Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Position::Left => "left",
                Position::Right => "right",
                Position::Top => "top",
                Position::Bottom => "bottom",
            }
        )
    }
}

impl TryFrom<&str> for Position {
    type Error = ();

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "left" => Ok(Position::Left),
            "right" => Ok(Position::Right),
            "top" => Ok(Position::Top),
            "bottom" => Ok(Position::Bottom),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// hostname of this client
    pub hostname: Option<String>,
    /// fix ips, determined by the user
    pub fix_ips: Vec<IpAddr>,
    /// both active_addr and addrs can be None / empty so port needs to be stored seperately
    pub port: u16,
    /// position of a client on screen
    pub pos: Position,
    /// enter hook
    pub cmd: Option<String>,
    /// Whether changes to this device's clipboard should be
    /// propagated to this peer. Per-pair gate on the *send* side;
    /// the receiving peer's `IncomingPeerConfig::clipboard_receive`
    /// is the matching gate on the receive side. Both must be true
    /// for clipboard text to flow. Defaults to `false` — clipboard
    /// is a meaningfully different trust scope than mouse/keyboard.
    #[serde(default)]
    pub clipboard_send: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            hostname: Default::default(),
            fix_ips: Default::default(),
            pos: Default::default(),
            cmd: None,
            clipboard_send: false,
        }
    }
}

pub type ClientHandle = u64;

/// Per-incoming-peer settings, keyed on the peer's TLS certificate
/// fingerprint in `Config::authorized_fingerprints`. Holds the
/// human-readable description plus receive-side post-processing
/// preferences applied to events forwarded from this peer.
///
/// Wire format is forward-compatible: legacy configs that store a
/// bare string per fingerprint deserialize as
/// `IncomingPeerConfig { description: <string>, .. defaults }`. The
/// custom `Deserialize` impl handles both shapes so users upgrading
/// from older versions don't lose their authorizations.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IncomingPeerConfig {
    pub description: String,
    /// Sign-flip scroll deltas before injection. Mirrors the
    /// libinput natural-scroll concept, but applies only to
    /// virtual-pointer events forwarded from this specific peer
    /// (which bypass libinput entirely on Wayland).
    pub natural_scroll: bool,
    /// Linear multiplier applied to motion deltas before injection.
    /// 1.0 = passthrough. Useful when senders capture motion at
    /// different scales (e.g. a Mac trackpad's small floating-point
    /// deltas vs a Windows high-DPI mouse's count deltas).
    pub mouse_sensitivity: f64,
    /// Most recent IP this peer connected from. Stored as a plain
    /// string (no port) so reconnects from the same machine on a
    /// new ephemeral port don't churn the value. Persists across
    /// daemon restarts so the GUI has something to show even when
    /// the peer is currently offline.
    pub last_addr: Option<String>,
    /// mDNS-resolved hostname for `last_addr` at the time of last
    /// connect, recovered from the discovery layer's
    /// `hostname → ip` cache. `None` if discovery wasn't running
    /// on either side or the peer's mDNS record didn't match the
    /// IP it connected from.
    pub last_hostname: Option<String>,
    /// Whether clipboard text propagated by this peer should be
    /// applied to the local clipboard. Per-pair gate on the
    /// *receive* side; the sending peer's
    /// `ClientConfig::clipboard_send` is the matching gate on the
    /// send side. Both must be true for clipboard text to flow.
    /// Defaults to `false`.
    pub clipboard_receive: bool,
}

impl Default for IncomingPeerConfig {
    fn default() -> Self {
        Self {
            description: String::new(),
            natural_scroll: false,
            mouse_sensitivity: 1.0,
            last_addr: None,
            last_hostname: None,
            clipboard_receive: false,
        }
    }
}

impl<'de> Deserialize<'de> for IncomingPeerConfig {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        // Legacy: a bare string is just the description.
        // Current: a struct with description + post-processing fields.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Legacy(String),
            Full {
                description: String,
                #[serde(default)]
                natural_scroll: bool,
                #[serde(default = "default_sensitivity")]
                mouse_sensitivity: f64,
                #[serde(default)]
                last_addr: Option<String>,
                #[serde(default)]
                last_hostname: Option<String>,
                #[serde(default)]
                clipboard_receive: bool,
            },
        }
        fn default_sensitivity() -> f64 {
            1.0
        }
        Ok(match Repr::deserialize(de)? {
            Repr::Legacy(description) => Self {
                description,
                ..Self::default()
            },
            Repr::Full {
                description,
                natural_scroll,
                mouse_sensitivity,
                last_addr,
                last_hostname,
                clipboard_receive,
            } => Self {
                description,
                natural_scroll,
                mouse_sensitivity,
                last_addr,
                last_hostname,
                clipboard_receive,
            },
        })
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ClientState {
    /// events should be sent to and received from the client
    pub active: bool,
    /// `active` address of the client, used to send data to.
    /// This should generally be the socket address where data
    /// was last received from.
    pub active_addr: Option<SocketAddr>,
    /// tracks whether or not the client is available for emulation
    pub alive: bool,
    /// ips from dns
    pub dns_ips: Vec<IpAddr>,
    /// all ip addresses associated with a particular client
    /// e.g. Laptops usually have at least an ethernet and a wifi port
    /// which have different ip addresses
    pub ips: HashSet<IpAddr>,
    /// client has pressed keys
    pub has_pressed_keys: bool,
    /// dns resolving in progress
    pub resolving: bool,
    /// Peer's build short commit hash from the [`Hello`] proto
    /// event. `None` means we haven't received a Hello yet — either
    /// the connection is fresh, or the peer is on an older build
    /// that predates the Hello event. The frontend uses this to
    /// soft-warn on version mismatch.
    pub peer_commit: Option<[u8; 8]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FrontendEvent {
    /// a client was created
    Created(ClientHandle, ClientConfig, ClientState),
    /// no such client
    NoSuchClient(ClientHandle),
    /// state changed
    State(ClientHandle, ClientConfig, ClientState),
    /// the client was deleted
    Deleted(ClientHandle),
    /// new port, reason of failure (if failed)
    PortChanged(u16, Option<String>),
    /// list of all clients, used for initial state synchronization
    Enumerate(Vec<(ClientHandle, ClientConfig, ClientState)>),
    /// an error occured
    Error(String),
    /// capture status
    CaptureStatus(Status),
    /// emulation status
    EmulationStatus(Status),
    /// authorized public key fingerprints have been updated.
    /// The map's value carries each peer's per-pair receive-side
    /// post-processing preferences alongside its description.
    AuthorizedUpdated(HashMap<String, IncomingPeerConfig>),
    /// public key fingerprint of this device
    PublicKeyFingerprint(String),
    /// new device connected
    DeviceConnected {
        addr: SocketAddr,
        fingerprint: String,
    },
    /// incoming device entered the screen
    DeviceEntered {
        fingerprint: String,
        addr: SocketAddr,
        pos: Position,
    },
    /// incoming disconnected
    IncomingDisconnected(SocketAddr),
    /// failed connection attempt (approval for fingerprint required)
    ConnectionAttempt { fingerprint: String },
    /// pixel threshold for the wall-press auto-release fallback.
    /// 0 means disabled.
    ReleaseThreshold(u32),
    /// whether mDNS-SD discovery is on. When true, lan-mouse
    /// advertises a `_lan-mouse._udp.local.` Bonjour service whose
    /// TXT record's `primary=` field hints at the OS-preferred
    /// interface (Mac service order / Linux default route), so
    /// peers can bias their connection attempts toward the right
    /// interface on multi-homed hosts.
    MdnsDiscovery(bool),
    /// Snapshot of the host-OS clipboard-suppression list. Pushed
    /// on Sync and after every Add/Remove so the GUI never has to
    /// query. The strings are opaque platform identifiers in the
    /// host's natural shape — bundle ID on macOS, exe basename on
    /// Windows, `xdg-toplevel.app_id` on Wayland, `WM_CLASS` on
    /// X11. Other-OS sections in `config.toml` are intentionally
    /// not surfaced — you edit those by sitting in front of that
    /// machine.
    SuppressedAppsUpdated(Vec<String>),
    /// Reply to [`FrontendRequest::ListRunningApps`]: best-effort
    /// list of apps currently running on this device. Empty when
    /// enumeration isn't reachable (no compositor support, missing
    /// permissions, transient race).
    RunningApps(Vec<RunningApp>),
}

/// One running-app candidate for the suppression-list picker. The
/// `identifier` is the host-OS string the runtime suppression check
/// compares against (bundle ID on macOS, exe basename on Windows,
/// `xdg-toplevel.app_id` on Wayland, `WM_CLASS` on X11). The
/// `display_name` is what we show in the dropdown — usually the
/// app's localized name on macOS, or just the identifier on
/// platforms where there's no separate user-facing label.
/// `icon_png` is an optional PNG-encoded icon (~32×32) the GUI can
/// render alongside the name; `None` on platforms where icon
/// extraction isn't implemented or when the OS reports no icon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningApp {
    pub display_name: String,
    pub identifier: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_png: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FrontendRequest {
    /// activate/deactivate client
    Activate(ClientHandle, bool),
    /// add a new client
    Create,
    /// change the listen port (recreate udp listener)
    ChangePort(u16),
    /// remove a client
    Delete(ClientHandle),
    /// request an enumeration of all clients
    Enumerate(),
    /// resolve dns
    ResolveDns(ClientHandle),
    /// update hostname
    UpdateHostname(ClientHandle, Option<String>),
    /// update port
    UpdatePort(ClientHandle, u16),
    /// update position
    UpdatePosition(ClientHandle, Position),
    /// update fix-ips
    UpdateFixIps(ClientHandle, Vec<IpAddr>),
    /// request reenabling input capture
    EnableCapture,
    /// request reenabling input emulation
    EnableEmulation,
    /// synchronize all state
    Sync,
    /// authorize fingerprint (description, fingerprint)
    AuthorizeKey(String, String),
    /// remove fingerprint (fingerprint)
    RemoveAuthorizedKey(String),
    /// change the hook command
    UpdateEnterHook(u64, Option<String>),
    /// save config file
    SaveConfiguration,
    /// set the wall-press auto-release pixel threshold (0 = disabled)
    SetReleaseThreshold(u32),
    /// set whether forwarded scroll events from a specific
    /// authorized peer should be sign-inverted on injection.
    /// Keyed on the peer's TLS certificate fingerprint.
    SetIncomingPeerNaturalScroll(String, bool),
    /// set the linear motion-sensitivity multiplier for events
    /// forwarded from a specific authorized peer. 1.0 = passthrough.
    /// Keyed on the peer's TLS certificate fingerprint.
    SetIncomingPeerSensitivity(String, f64),
    /// turn mDNS-SD discovery on or off
    SetMdnsDiscovery(bool),
    /// Toggle whether clipboard changes on this device propagate to
    /// the given outgoing client. Per-pair send-side gate.
    SetClientClipboardSend(ClientHandle, bool),
    /// Toggle whether clipboard text from the given authorized peer
    /// is applied to this device's clipboard. Per-pair receive-side
    /// gate, keyed on the peer's TLS certificate fingerprint.
    SetIncomingPeerClipboardReceive(String, bool),
    /// Add a host-OS app identifier to the clipboard suppression
    /// list — that app's clipboard contents will never be
    /// broadcast to peers. The kind is implicit from the OS the
    /// daemon is running on (see [`host_app_ident`]). Idempotent.
    AddSuppressedApp(String),
    /// Remove a host-OS app identifier from the clipboard
    /// suppression list. Idempotent.
    RemoveSuppressedApp(String),
    /// Ask the daemon to enumerate currently-running apps so the
    /// "From running apps" tab in the suppression-list modal can be
    /// populated. The daemon replies with a
    /// [`FrontendEvent::RunningApps`].
    ListRunningApps,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Status {
    #[default]
    Disabled,
    Enabled,
}

impl From<Status> for bool {
    fn from(status: Status) -> Self {
        match status {
            Status::Enabled => true,
            Status::Disabled => false,
        }
    }
}

#[cfg(unix)]
const LAN_MOUSE_SOCKET_NAME: &str = "lan-mouse-socket.sock";

#[derive(Debug, Error)]
pub enum SocketPathError {
    #[error("could not determine $XDG_RUNTIME_DIR: `{0}`")]
    XdgRuntimeDirNotFound(VarError),
    #[error("could not determine $HOME: `{0}`")]
    HomeDirNotFound(VarError),
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn default_socket_path() -> Result<PathBuf, SocketPathError> {
    let xdg_runtime_dir =
        env::var("XDG_RUNTIME_DIR").map_err(SocketPathError::XdgRuntimeDirNotFound)?;
    Ok(Path::new(xdg_runtime_dir.as_str()).join(LAN_MOUSE_SOCKET_NAME))
}

#[cfg(all(unix, target_os = "macos"))]
pub fn default_socket_path() -> Result<PathBuf, SocketPathError> {
    let home = env::var("HOME").map_err(SocketPathError::HomeDirNotFound)?;
    Ok(Path::new(home.as_str())
        .join("Library")
        .join("Caches")
        .join(LAN_MOUSE_SOCKET_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ident_matches_within_variant_case_insensitive() {
        let entry = AppIdent::WindowsExe("1Password.exe".into());
        let detected = AppIdent::WindowsExe("1password.exe".into());
        assert!(entry.matches(&detected));
        assert!(detected.matches(&entry));
    }

    #[test]
    fn app_ident_matches_rejects_value_mismatch() {
        let a = AppIdent::LinuxX11("firefox".into());
        let b = AppIdent::LinuxX11("chromium".into());
        assert!(!a.matches(&b));
    }

    #[test]
    fn app_ident_matches_rejects_cross_variant() {
        // A Mac entry for `org.mozilla.firefox` must NOT suppress a
        // Linux peer that reports `firefox` as its WM_CLASS — same
        // semantic app, different platform identifiers, and the
        // suppression list lives on the device that captures the
        // clipboard, not the device that displays the window.
        let mac = AppIdent::MacBundle("org.mozilla.firefox".into());
        let linux = AppIdent::LinuxX11("firefox".into());
        assert!(!mac.matches(&linux));
    }

    #[test]
    fn app_ident_serde_round_trip_json() {
        let cases = [
            AppIdent::MacBundle("com.1password.1password7".into()),
            AppIdent::WindowsExe("1password.exe".into()),
            AppIdent::LinuxX11("firefox".into()),
            AppIdent::LinuxWayland("org.mozilla.firefox".into()),
        ];
        for original in cases {
            let s = serde_json::to_string(&original).expect("encode");
            let decoded: AppIdent = serde_json::from_str(&s).expect("decode");
            assert_eq!(original, decoded, "round-trip mismatch: {s}");
        }
    }

    #[test]
    fn app_ident_serde_kind_tag_is_snake_case() {
        // Pin the tag rendering — config.toml + IPC consumers depend
        // on these strings, so an accidental rename in a serde
        // attribute would silently break legacy configs.
        let mac = AppIdent::MacBundle("x".into());
        assert_eq!(
            serde_json::to_string(&mac).unwrap(),
            r#"{"kind":"mac_bundle","value":"x"}"#
        );
        let exe = AppIdent::WindowsExe("y".into());
        assert_eq!(
            serde_json::to_string(&exe).unwrap(),
            r#"{"kind":"windows_exe","value":"y"}"#
        );
        let x11 = AppIdent::LinuxX11("z".into());
        assert_eq!(
            serde_json::to_string(&x11).unwrap(),
            r#"{"kind":"linux_x11","value":"z"}"#
        );
        let wl = AppIdent::LinuxWayland("w".into());
        assert_eq!(
            serde_json::to_string(&wl).unwrap(),
            r#"{"kind":"linux_wayland","value":"w"}"#
        );
    }

    #[test]
    fn app_ident_label_includes_platform() {
        assert!(AppIdent::MacBundle("com.x.y".into())
            .label()
            .contains("macOS bundle"));
        assert!(AppIdent::WindowsExe("z.exe".into())
            .label()
            .contains("Windows"));
        assert!(AppIdent::LinuxX11("z".into()).label().contains("X11"));
        assert!(AppIdent::LinuxWayland("z".into())
            .label()
            .contains("Wayland"));
    }

    #[test]
    fn incoming_peer_legacy_string_deserializes() {
        // Configs from before the per-pair scroll/sensitivity work
        // store each authorized peer as a bare description string.
        // Custom Deserialize must accept that shape so upgrading
        // doesn't silently drop authorizations.
        let json = r#""my laptop""#;
        let peer: IncomingPeerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(peer.description, "my laptop");
        assert!(!peer.natural_scroll);
        assert!(!peer.clipboard_receive);
        assert_eq!(peer.mouse_sensitivity, 1.0);
    }

    #[test]
    fn incoming_peer_full_with_clipboard_receive_round_trips() {
        let original = IncomingPeerConfig {
            description: "laptop".into(),
            natural_scroll: true,
            mouse_sensitivity: 1.5,
            last_addr: Some("10.0.0.1".into()),
            last_hostname: Some("foo.local".into()),
            clipboard_receive: true,
        };
        let s = serde_json::to_string(&original).unwrap();
        let decoded: IncomingPeerConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn incoming_peer_legacy_full_without_clipboard_receive_defaults_false() {
        // A v0 "Full" entry (struct shape but no clipboard_receive
        // field) must default the new field to false rather than
        // refusing to decode.
        let json = r#"{
            "description": "laptop",
            "natural_scroll": true,
            "mouse_sensitivity": 1.5,
            "last_addr": "10.0.0.1",
            "last_hostname": "foo.local"
        }"#;
        let peer: IncomingPeerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(peer.description, "laptop");
        assert!(peer.natural_scroll);
        assert_eq!(peer.mouse_sensitivity, 1.5);
        assert!(!peer.clipboard_receive);
    }

    #[test]
    fn client_config_clipboard_send_defaults_false() {
        // ClientConfig uses derive(Deserialize) with
        // #[serde(default)] on clipboard_send — a JSON object that
        // omits the field should still decode and produce false.
        let json = r#"{
            "hostname": null,
            "fix_ips": [],
            "port": 4242,
            "pos": "left",
            "cmd": null
        }"#;
        let cfg: ClientConfig = serde_json::from_str(json).unwrap();
        assert!(!cfg.clipboard_send);
    }
}
