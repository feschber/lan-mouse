use std::io;

use futures_core::Stream;

use crate::{
    client::{ClientEvent, ClientHandle},
    event::Event,
};

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

pub async fn create() -> Box<dyn InputCapture<Item = io::Result<(ClientHandle, Event)>>> {
    #[cfg(target_os = "macos")]
    match macos::MacOSInputCapture::new() {
        Ok(p) => return Box::new(p),
        Err(e) => log::info!("macos input capture not available: {e}"),
    }

    #[cfg(windows)]
    match windows::WindowsInputCapture::new() {
        Ok(p) => return Box::new(p),
        Err(e) => log::info!("windows input capture not available: {e}"),
    }

    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    match libei::LibeiInputCapture::new().await {
        Ok(p) => {
            log::info!("using libei input capture");
            return Box::new(p);
        }
        Err(e) => log::info!("libei input capture not available: {e}"),
    }

    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    match wayland::WaylandInputCapture::new() {
        Ok(p) => {
            log::info!("using layer-shell input capture");
            return Box::new(p);
        }
        Err(e) => log::info!("layer_shell input capture not available: {e}"),
    }

    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    match x11::X11InputCapture::new() {
        Ok(p) => {
            log::info!("using x11 input capture");
            return Box::new(p);
        }
        Err(e) => log::info!("x11 input capture not available: {e}"),
    }

    log::error!("falling back to dummy input capture");
    Box::new(dummy::DummyInputCapture::new())
}

pub trait InputCapture: Stream<Item = io::Result<(ClientHandle, Event)>> + Unpin {
    /// notify input capture of configuration changes
    fn notify(&mut self, event: ClientEvent) -> io::Result<()>;

    /// release mouse
    fn release(&mut self) -> io::Result<()>;
}
