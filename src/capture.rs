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

#[allow(unreachable_code)]
pub async fn create(
    backend: Option<CaptureBackend>,
) -> Result<Box<dyn InputCapture<Item = io::Result<(ClientHandle, Event)>>>, CaptureCreationError> {
    if let Some(backend) = backend {
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

    #[cfg(target_os = "macos")]
    match macos::MacOSInputCapture::new() {
        Ok(p) => return Ok(Box::new(p)),
        Err(e) => log::info!("macos input capture not available: {e}"),
    }

    #[cfg(windows)]
    return Ok(Box::new(windows::WindowsInputCapture::new()));

    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    match libei::LibeiInputCapture::new().await {
        Ok(p) => {
            log::info!("using libei input capture");
            return Ok(Box::new(p));
        }
        Err(e) => log::info!("libei input capture not available: {e}"),
    }

    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    match wayland::WaylandInputCapture::new() {
        Ok(p) => {
            log::info!("using layer-shell input capture");
            return Ok(Box::new(p));
        }
        Err(e) => log::info!("layer_shell input capture not available: {e}"),
    }

    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    match x11::X11InputCapture::new() {
        Ok(p) => {
            log::info!("using x11 input capture");
            return Ok(Box::new(p));
        }
        Err(e) => log::info!("x11 input capture not available: {e}"),
    }

    log::error!("falling back to dummy input capture");
    Ok(Box::new(dummy::DummyInputCapture::new()))
}

pub trait InputCapture: Stream<Item = io::Result<(ClientHandle, Event)>> + Unpin {
    /// notify input capture of configuration changes
    fn notify(&mut self, event: ClientEvent) -> io::Result<()>;

    /// release mouse
    fn release(&mut self) -> io::Result<()>;
}
