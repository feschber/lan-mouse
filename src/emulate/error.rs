use std::{fmt::Display, io};

use thiserror::Error;
use wayland_client::{
    backend::WaylandError,
    globals::{BindError, GlobalError},
    ConnectError, DispatchError,
};

#[derive(Debug, Error)]
pub enum EmulationCreationError {
    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    Wlroots(#[from] WlrootsEmulationCreationError),
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    Libei(#[from] LibeiEmulationCreationError),
    #[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
    Xdp(#[from] XdpEmulationCreationError),
    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    X11(#[from] X11EmulationCreationError),
}

impl Display for EmulationCreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self {
            EmulationCreationError::Wlroots(e) => format!("wlroots backend: {e}"),
            EmulationCreationError::Libei(e) => format!("libei backend: {e}"),
            EmulationCreationError::Xdp(e) => format!("desktop portal backend: {e}"),
            EmulationCreationError::X11(e) => format!("x11 backend: {e}"),
        };
        write!(f, "could not create input emulation backend: {reason}")
    }
}

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum WlrootsEmulationCreationError {
    Connect(#[from] ConnectError),
    Global(#[from] GlobalError),
    Wayland(#[from] WaylandError),
    Bind(#[from] WaylandBindError),
    Dispatch(#[from] DispatchError),
    Io(#[from] io::Error),
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
impl Display for WlrootsEmulationCreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WlrootsEmulationCreationError::Bind(e) => write!(f, "{e}"),
            WlrootsEmulationCreationError::Connect(e) => {
                write!(f, "could not connect to wayland compositor: {e}")
            }
            WlrootsEmulationCreationError::Global(e) => write!(f, "wayland error: {e}"),
            WlrootsEmulationCreationError::Wayland(e) => write!(f, "wayland error: {e}"),
            WlrootsEmulationCreationError::Dispatch(e) => {
                write!(f, "error dispatching wayland events: {e}")
            }
            WlrootsEmulationCreationError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum LibeiEmulationCreationError {
    Ashpd(#[from] ashpd::Error),
    Io(#[from] io::Error),
}

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
impl Display for LibeiEmulationCreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LibeiEmulationCreationError::Ashpd(e) => write!(f, "xdg-desktop-portal: {e}"),
            LibeiEmulationCreationError::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

#[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum XdpEmulationCreationError {}

#[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
#[derive(Debug, Error)]
pub enum X11EmulationCreationError {
    OpenDisplay,
}

#[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
impl Display for X11EmulationCreationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            X11EmulationCreationError::OpenDisplay => write!(f, "could not open display!"),
        }
    }
}
