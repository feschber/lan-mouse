use std::{fmt::Display, io};

use thiserror::Error;
use wayland_client::{
    backend::WaylandError,
    globals::{BindError, GlobalError},
    ConnectError, DispatchError,
};

#[derive(Debug, Error)]
pub enum CaptureCreationError {
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    Libei(#[from] LibeiCaptureCreationError),
    LayerShell(#[from] LayerShellCaptureCreationError),
    X11(#[from] X11InputCaptureCreationError),
}

impl Display for CaptureCreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
            CaptureCreationError::Libei(reason) => {
                format!("error creating portal backend: {reason}")
            }
            CaptureCreationError::LayerShell(reason) => {
                format!("error creating layer-shell backend: {reason}")
            }
            CaptureCreationError::X11(e) => format!("{e}"),
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

#[derive(Debug, Error)]
pub struct WaylandBindError {
    inner: BindError,
    protocol: &'static str,
}
impl WaylandBindError {
    pub(crate) fn new(inner: BindError, protocol: &'static str) -> Self {
        Self { inner, protocol }
    }
}

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

#[derive(Debug, Error)]
pub enum X11InputCaptureCreationError {
    NotImplemented,
}

impl Display for X11InputCaptureCreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "X11 input capture is not yet implemented :(")
    }
}
