use async_trait::async_trait;
use std::fmt::Display;

use input_event::Event;

use anyhow::Result;

use self::error::EmulationCreationError;

#[cfg(windows)]
pub mod windows;

#[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
pub mod x11;

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
pub mod wlroots;

#[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
pub mod xdg_desktop_portal;

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
pub mod libei;

#[cfg(target_os = "macos")]
pub mod macos;

/// fallback input emulation (logs events)
pub mod dummy;
pub mod error;

pub type EmulationHandle = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    Wlroots,
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    Libei,
    #[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
    Xdp,
    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    X11,
    #[cfg(windows)]
    Windows,
    #[cfg(target_os = "macos")]
    MacOs,
    Dummy,
}

impl Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
            Backend::Wlroots => write!(f, "wlroots"),
            #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
            Backend::Libei => write!(f, "libei"),
            #[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
            Backend::Xdp => write!(f, "xdg-desktop-portal"),
            #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
            Backend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            Backend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            Backend::MacOs => write!(f, "macos"),
            Backend::Dummy => write!(f, "dummy"),
        }
    }
}

#[async_trait]
pub trait InputEmulation: Send {
    async fn consume(&mut self, event: Event, handle: EmulationHandle);
    async fn create(&mut self, handle: EmulationHandle);
    async fn destroy(&mut self, handle: EmulationHandle);
}

pub async fn create_backend(
    backend: Backend,
) -> Result<Box<dyn InputEmulation>, EmulationCreationError> {
    match backend {
        #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
        Backend::Wlroots => Ok(Box::new(wlroots::WlrootsEmulation::new()?)),
        #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
        Backend::Libei => Ok(Box::new(libei::LibeiEmulation::new().await?)),
        #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
        Backend::X11 => Ok(Box::new(x11::X11Emulation::new()?)),
        #[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
        Backend::Xdp => Ok(Box::new(
            xdg_desktop_portal::DesktopPortalEmulation::new().await?,
        )),
        #[cfg(windows)]
        Backend::Windows => Ok(Box::new(windows::WindowsEmulation::new()?)),
        #[cfg(target_os = "macos")]
        Backend::MacOs => Ok(Box::new(macos::MacOSEmulation::new()?)),
        Backend::Dummy => Ok(Box::new(dummy::DummyEmulation::new())),
    }
}

pub async fn create(
    backend: Option<Backend>,
) -> Result<Box<dyn InputEmulation>, EmulationCreationError> {
    if let Some(backend) = backend {
        let b = create_backend(backend).await;
        if b.is_ok() {
            log::info!("using emulation backend: {backend}");
        }
        return b;
    }

    for backend in [
        #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
        Backend::Wlroots,
        #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
        Backend::Libei,
        #[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
        Backend::Xdp,
        #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
        Backend::X11,
        #[cfg(windows)]
        Backend::Windows,
        #[cfg(target_os = "macos")]
        Backend::MacOs,
        Backend::Dummy,
    ] {
        match create_backend(backend).await {
            Ok(b) => {
                log::info!("using emulation backend: {backend}");
                return Ok(b);
            }
            Err(e) => log::warn!("{e}"),
        }
    }

    Err(EmulationCreationError::NoAvailableBackend)
}
