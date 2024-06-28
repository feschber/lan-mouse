use thiserror::Error;
use std::fmt::Display;

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
use wayland_client::{
    backend::WaylandError,
    globals::{BindError, GlobalError},
    ConnectError, DispatchError,
};
#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
use std::io;

#[derive(Debug, Error)]
pub enum CaptureCreationError {
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    Libei(#[from] LibeiCaptureCreationError),
    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    LayerShell(#[from] LayerShellCaptureCreationError),
    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    X11(#[from] X11InputCaptureCreationError),
    #[cfg(target_os = "macos")]
    Macos(#[from] MacOSInputCaptureCreationError),
    #[cfg(windows)]
    Windows,
}

impl Display for CaptureCreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
            CaptureCreationError::Libei(reason) => {
                format!("error creating portal backend: {reason}")
            }
            #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
            CaptureCreationError::LayerShell(reason) => {
                format!("error creating layer-shell backend: {reason}")
            }
            #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
            CaptureCreationError::X11(e) => format!("{e}"),
            #[cfg(target_os = "macos")]
            CaptureCreationError::Macos(e) => format!("{e}"),
            #[cfg(windows)]
            CaptureCreationError::Windows => String::from(""),
        };
        write!(f, "could not create input capture: {reason}")
    }
}

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum LibeiCaptureCreationError {
    Ashpd(#[from] ashpd::Error),
}

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
impl Display for LibeiCaptureCreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LibeiCaptureCreationError::Ashpd(portal_error) => write!(f, "{portal_error}"),
        }
    }
}

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
#[derive(Debug, Error)]
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
impl Display for WaylandBindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} protocol not supported: {}",
            self.protocol, self.inner
        )
    }
}

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum LayerShellCaptureCreationError {
    Connect(#[from] ConnectError),
    Global(#[from] GlobalError),
    Wayland(#[from] WaylandError),
    Bind(#[from] WaylandBindError),
    Dispatch(#[from] DispatchError),
    Io(#[from] io::Error),
}

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
impl Display for LayerShellCaptureCreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayerShellCaptureCreationError::Bind(e) => write!(f, "{e}"),
            LayerShellCaptureCreationError::Connect(e) => {
                write!(f, "could not connect to wayland compositor: {e}")
            }
            LayerShellCaptureCreationError::Global(e) => write!(f, "wayland error: {e}"),
            LayerShellCaptureCreationError::Wayland(e) => write!(f, "wayland error: {e}"),
            LayerShellCaptureCreationError::Dispatch(e) => {
                write!(f, "error dispatching wayland events: {e}")
            }
            LayerShellCaptureCreationError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

#[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum X11InputCaptureCreationError {
    NotImplemented,
}

#[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
impl Display for X11InputCaptureCreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "X11 input capture is not yet implemented :(")
    }
}
#[cfg(target_os = "macos")]
#[derive(Debug, Error)]
pub enum MacOSInputCaptureCreationError {
    NotImplemented,
}

#[cfg(target_os = "macos")]
impl Display for MacOSInputCaptureCreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "macos input capture is not yet implemented :(")
    }
}
