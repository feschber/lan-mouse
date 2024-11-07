use thiserror::Error;

#[derive(Debug, Error)]
pub enum InputCaptureError {
    #[error("error creating input-capture: `{0}`")]
    Create(#[from] CaptureCreationError),
    #[error("error while capturing input: `{0}`")]
    Capture(#[from] CaptureError),
}

#[cfg(all(unix, feature = "layer_shell", not(target_os = "macos")))]
use std::io;
#[cfg(all(unix, feature = "layer_shell", not(target_os = "macos")))]
use wayland_client::{
    backend::WaylandError,
    globals::{BindError, GlobalError},
    ConnectError, DispatchError,
};

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
use ashpd::desktop::ResponseError;

#[cfg(target_os = "macos")]
use core_graphics::base::CGError;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("activation stream closed unexpectedly")]
    ActivationClosed,
    #[error("libei stream was closed")]
    EndOfStream,
    #[error("io error: `{0}`")]
    Io(#[from] std::io::Error),
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    #[error("libei error: `{0}`")]
    Reis(#[from] reis::Error),
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    #[error(transparent)]
    Portal(#[from] ashpd::Error),
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    #[error("libei disconnected - reason: `{0}`")]
    Disconnected(String),
    #[cfg(target_os = "macos")]
    #[error("failed to warp mouse cursor: `{0}`")]
    WarpCursor(CGError),
    #[cfg(target_os = "macos")]
    #[error("reset_mouse_position called without a connected client")]
    ResetMouseWithoutClient,
    #[cfg(target_os = "macos")]
    #[error("core-graphics error: {0}")]
    CoreGraphics(CGError),
    #[cfg(target_os = "macos")]
    #[error("unable to map key event: {0}")]
    KeyMapError(i64),
    #[cfg(target_os = "macos")]
    #[error("Event tap disabled")]
    EventTapDisabled,
}

#[derive(Debug, Error)]
pub enum CaptureCreationError {
    #[error("no backend available")]
    NoAvailableBackend,
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    #[error("error creating input-capture-portal backend: `{0}`")]
    Libei(#[from] LibeiCaptureCreationError),
    #[cfg(all(unix, feature = "layer_shell", not(target_os = "macos")))]
    #[error("error creating layer-shell capture backend: `{0}`")]
    LayerShell(#[from] LayerShellCaptureCreationError),
    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    #[error("error creating x11 capture backend: `{0}`")]
    X11(#[from] X11InputCaptureCreationError),
    #[cfg(windows)]
    #[error("error creating windows capture backend")]
    Windows,
    #[cfg(target_os = "macos")]
    #[error("error creating macos capture backend: `{0}`")]
    MacOS(#[from] MacosCaptureCreationError),
}

impl CaptureCreationError {
    /// request was intentionally denied by the user
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    pub(crate) fn cancelled_by_user(&self) -> bool {
        matches!(
            self,
            CaptureCreationError::Libei(LibeiCaptureCreationError::Ashpd(ashpd::Error::Response(
                ResponseError::Cancelled
            )))
        )
    }
    #[cfg(not(all(unix, feature = "libei", not(target_os = "macos"))))]
    pub(crate) fn cancelled_by_user(&self) -> bool {
        false
    }
}

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum LibeiCaptureCreationError {
    #[error("xdg-desktop-portal: `{0}`")]
    Ashpd(#[from] ashpd::Error),
}

#[cfg(all(unix, feature = "layer_shell", not(target_os = "macos")))]
#[derive(Debug, Error)]
#[error("{protocol} protocol not supported: {inner}")]
pub struct WaylandBindError {
    inner: BindError,
    protocol: &'static str,
}

#[cfg(all(unix, feature = "layer_shell", not(target_os = "macos")))]
impl WaylandBindError {
    pub(crate) fn new(inner: BindError, protocol: &'static str) -> Self {
        Self { inner, protocol }
    }
}

#[cfg(all(unix, feature = "layer_shell", not(target_os = "macos")))]
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
pub enum MacosCaptureCreationError {
    #[error("event source creation failed!")]
    EventSourceCreation,
    #[cfg(target_os = "macos")]
    #[error("event tap creation failed")]
    EventTapCreation,
    #[error("failed to set CG Cursor property")]
    CGCursorProperty,
    #[cfg(target_os = "macos")]
    #[error("failed to get display ids: {0}")]
    ActiveDisplays(CGError),
}
