use thiserror::Error;

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
use std::io;
#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
use wayland_client::{
    backend::WaylandError,
    globals::{BindError, GlobalError},
    ConnectError, DispatchError,
};

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
use reis::tokio::{EiConvertEventStreamError, HandshakeError};

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
#[derive(Debug, Error)]
#[error("error in libei stream: {inner:?}")]
pub struct ReisConvertEventStreamError {
    inner: EiConvertEventStreamError,
}

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
impl From<EiConvertEventStreamError> for ReisConvertEventStreamError {
    fn from(e: EiConvertEventStreamError) -> Self {
        Self { inner: e }
    }
}

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("activation stream closed unexpectedly")]
    ActivationClosed,
    #[error("libei stream was closed")]
    EndOfStream,
    #[error("io error: `{0}`")]
    Io(#[from] std::io::Error),
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    #[error("error in libei stream: `{0}`")]
    Reis(#[from] ReisConvertEventStreamError),
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    #[error("libei handshake failed: `{0}`")]
    Handshake(#[from] HandshakeError),
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    #[error(transparent)]
    Portal(#[from] ashpd::Error),
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    #[error("libei disconnected - reason: `{0}`")]
    Disconnected(String),
}

#[derive(Debug, Error)]
pub enum CaptureCreationError {
    #[error("no backend available")]
    NoAvailableBackend,
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    #[error("error creating input-capture-portal backend: `{0}`")]
    Libei(#[from] LibeiCaptureCreationError),
    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    #[error("error creating layer-shell capture backend: `{0}`")]
    LayerShell(#[from] LayerShellCaptureCreationError),
    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    #[error("error creating x11 capture backend: `{0}`")]
    X11(#[from] X11InputCaptureCreationError),
    #[cfg(target_os = "macos")]
    #[error("error creating macos capture backend: `{0}`")]
    Macos(#[from] MacOSInputCaptureCreationError),
    #[cfg(windows)]
    #[error("error creating windows capture backend")]
    Windows,
}

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum LibeiCaptureCreationError {
    #[error("xdg-desktop-portal: `{0}`")]
    Ashpd(#[from] ashpd::Error),
}

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
#[derive(Debug, Error)]
#[error("{protocol} protocol not supported: {inner}")]
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

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum LayerShellCaptureCreationError {
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
    Io(#[from] io::Error),
}

#[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum X11InputCaptureCreationError {
    #[error("X11 input capture is not yet implemented :(")]
    NotImplemented,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Error)]
pub enum MacOSInputCaptureCreationError {
    #[error("MacOS input capture is not yet implemented :(")]
    NotImplemented,
}
