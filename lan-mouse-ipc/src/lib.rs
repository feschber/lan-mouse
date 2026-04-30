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
mod gui_lock;
mod listen;

pub use connect::{FrontendEventReader, FrontendRequestWriter, connect};
pub use connect_async::{AsyncFrontendEventReader, AsyncFrontendRequestWriter, connect_async};
pub use gui_lock::{GuiLock, GuiLockError};
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
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            hostname: Default::default(),
            fix_ips: Default::default(),
            pos: Default::default(),
            cmd: None,
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
}

impl Default for IncomingPeerConfig {
    fn default() -> Self {
        Self {
            description: String::new(),
            natural_scroll: false,
            mouse_sensitivity: 1.0,
            last_addr: None,
            last_hostname: None,
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
            } => Self {
                description,
                natural_scroll,
                mouse_sensitivity,
                last_addr,
                last_hostname,
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
