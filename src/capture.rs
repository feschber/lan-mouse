use std::io;

use futures_core::Stream;

use crate::{
    client::{ClientEvent, ClientHandle},
    config::CaptureBackend,
    event::Event,
};

use self::error::CaptureCreationError;

pub mod error;

#[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
pub mod libei;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
pub mod wayland;

#[cfg(windows)]
pub mod windows;

#[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
pub mod x11;

/// fallback input capture (does not produce events)
pub mod dummy;

pub async fn create_backend(
    backend: CaptureBackend,
) -> Result<Box<dyn InputCapture<Item = io::Result<(ClientHandle, Event)>>>, CaptureCreationError> {
    return match backend {
        #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
        CaptureBackend::InputCapturePortal => Ok(Box::new(libei::LibeiInputCapture::new().await?)),
        #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
        CaptureBackend::LayerShell => Ok(Box::new(wayland::WaylandInputCapture::new()?)),
        #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
        CaptureBackend::X11 => Ok(Box::new(x11::X11InputCapture::new()?)),
        #[cfg(windows)]
        CaptureBackend::Windows => Ok(Box::new(windows::WindowsInputCapture::new())),
        #[cfg(target_os = "macos")]
        CaptureBackend::MacOs => Ok(Box::new(macos::MacOSInputCapture::new()?)),
        CaptureBackend::Dummy => Ok(Box::new(dummy::DummyInputCapture::new())),
    };
}

pub async fn create(
    backend: Option<CaptureBackend>,
) -> Result<Box<dyn InputCapture<Item = io::Result<(ClientHandle, Event)>>>, CaptureCreationError> {
    if let Some(backend) = backend {
        let b = create_backend(backend).await;
        if b.is_ok() {
            log::info!("using capture backend: {backend}");
        }
        return b;
    }

    for backend in [
        #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
        CaptureBackend::InputCapturePortal,
        #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
        CaptureBackend::LayerShell,
        #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
        CaptureBackend::X11,
        #[cfg(windows)]
        CaptureBackend::Windows,
        #[cfg(target_os = "macos")]
        CaptureBackend::MacOs,
        CaptureBackend::Dummy,
    ] {
        match create_backend(backend).await {
            Ok(b) => {
                log::info!("using capture backend: {backend}");
                return Ok(b);
            }
            Err(e) => log::warn!("{backend} input capture backend unavailable: {e}"),
        }
    }
    Err(CaptureCreationError::NoAvailableBackend)
}

pub trait InputCapture: Stream<Item = io::Result<(ClientHandle, Event)>> + Unpin {
    /// notify input capture of configuration changes
    fn notify(&mut self, event: ClientEvent) -> io::Result<()>;

    /// release mouse
    fn release(&mut self) -> io::Result<()>;
}
