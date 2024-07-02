use async_trait::async_trait;
use std::future;

use crate::{config::EmulationBackend, event::Event};
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

#[async_trait]
pub trait InputEmulation: Send {
    async fn consume(&mut self, event: Event, handle: EmulationHandle);
    async fn create(&mut self, handle: EmulationHandle);
    async fn destroy(&mut self, handle: EmulationHandle);
    /// this function is waited on continuously and can be used to handle events
    async fn dispatch(&mut self) -> Result<()> {
        let _: () = future::pending().await;
        Ok(())
    }
}

pub async fn create_backend(
    backend: EmulationBackend,
) -> Result<Box<dyn InputEmulation>, EmulationCreationError> {
    match backend {
        #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
        EmulationBackend::Wlroots => Ok(Box::new(wlroots::WlrootsEmulation::new()?)),
        #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
        EmulationBackend::Libei => Ok(Box::new(libei::LibeiEmulation::new().await?)),
        #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
        EmulationBackend::X11 => Ok(Box::new(x11::X11Emulation::new()?)),
        #[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
        EmulationBackend::Xdp => Ok(Box::new(
            xdg_desktop_portal::DesktopPortalEmulation::new().await?,
        )),
        #[cfg(windows)]
        EmulationBackend::Windows => Ok(Box::new(windows::WindowsEmulation::new()?)),
        #[cfg(target_os = "macos")]
        EmulationBackend::MacOs => Ok(Box::new(macos::MacOSEmulation::new()?)),
        EmulationBackend::Dummy => Ok(Box::new(dummy::DummyEmulation::new())),
    }
}

pub async fn create(
    backend: Option<EmulationBackend>,
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
        EmulationBackend::Wlroots,
        #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
        EmulationBackend::Libei,
        #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
        EmulationBackend::X11,
        #[cfg(windows)]
        EmulationBackend::Windows,
        #[cfg(target_os = "macos")]
        EmulationBackend::MacOs,
        EmulationBackend::Dummy,
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
