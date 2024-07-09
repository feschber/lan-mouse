#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
use reis::tokio::EiConvertEventStreamError;
use std::io;
use thiserror::Error;

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
use wayland_client::{
    backend::WaylandError,
    globals::{BindError, GlobalError},
    ConnectError, DispatchError,
};

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
use reis::tokio::HandshakeError;

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
#[derive(Debug, Error)]
#[error("error in libei stream: {inner:?}")]
pub struct ReisConvertStreamError {
    inner: EiConvertEventStreamError,
}

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
impl From<EiConvertEventStreamError> for ReisConvertStreamError {
    fn from(e: EiConvertEventStreamError) -> Self {
        Self { inner: e }
    }
}

#[derive(Debug, Error)]
pub enum EmulationError {
    #[error("event stream closed")]
    EndOfStream,
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    #[error("libei error flushing events: `{0}`")]
    Libei(#[from] reis::event::Error),
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    #[error("")]
    LibeiConvertStream(#[from] ReisConvertStreamError),
    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    #[error("wayland error: `{0}`")]
    Wayland(#[from] wayland_client::backend::WaylandError),
    #[cfg(all(
        unix,
        any(feature = "xdg_desktop_portal", feature = "libei"),
        not(target_os = "macos")
    ))]
    #[error("xdg-desktop-portal: `{0}`")]
    Ashpd(#[from] ashpd::Error),
    #[error("io error: `{0}`")]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum EmulationCreationError {
    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    #[error("wlroots backend: `{0}`")]
    Wlroots(#[from] WlrootsEmulationCreationError),
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    #[error("libei backend: `{0}`")]
    Libei(#[from] LibeiEmulationCreationError),
    #[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
    #[error("xdg-desktop-portal: `{0}`")]
    Xdp(#[from] XdpEmulationCreationError),
    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    #[error("x11: `{0}`")]
    X11(#[from] X11EmulationCreationError),
    #[cfg(target_os = "macos")]
    #[error("macos: `{0}`")]
    MacOs(#[from] MacOSEmulationCreationError),
    #[cfg(windows)]
    #[error("windows: `{0}`")]
    Windows(#[from] WindowsEmulationCreationError),
    #[error("capture error")]
    NoAvailableBackend,
}

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum WlrootsEmulationCreationError {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Global(#[from] GlobalError),
    #[error(transparent)]
    Wayland(#[from] WaylandError),
    #[error(transparent)]
    Bind(#[from] WaylandBindError),
    #[error(transparent)]
    Dispatch(#[from] DispatchError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
#[derive(Debug, Error)]
#[error("wayland protocol \"{protocol}\" not supported: {inner}")]
pub struct WaylandBindError {
    inner: BindError,
    protocol: &'static str,
}

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
impl WaylandBindError {
    pub(crate) fn new(inner: BindError, protocol: &'static str) -> Self {
        Self { inner, protocol }
    }
}

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum LibeiEmulationCreationError {
    #[error(transparent)]
    Ashpd(#[from] ashpd::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Handshake(#[from] HandshakeError),
}

#[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum XdpEmulationCreationError {
    #[error(transparent)]
    Ashpd(#[from] ashpd::Error),
}

#[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum X11EmulationCreationError {
    #[error("could not open display")]
    OpenDisplay,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Error)]
pub enum MacOSEmulationCreationError {
    #[error("could not create event source")]
    EventSourceCreation,
}

#[cfg(windows)]
#[derive(Debug, Error)]
pub enum WindowsEmulationCreationError {}
