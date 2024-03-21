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

/// fallback event producer
pub mod dummy;

pub async fn create() -> Box<dyn EventProducer> {
    #[cfg(target_os = "macos")]
    match macos::MacOSProducer::new() {
        Ok(p) => return Box::new(p),
        Err(e) => log::info!("macos event producer not available: {e}"),
    }

    #[cfg(windows)]
    match windows::WindowsProducer::new() {
        Ok(p) => return Box::new(p),
        Err(e) => log::info!("windows event producer not available: {e}"),
    }

    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    match libei::LibeiProducer::new().await {
        Ok(p) => {
            log::info!("using libei event producer");
            return Box::new(p);
        }
        Err(e) => log::info!("libei event producer not available: {e}"),
    }

    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    match wayland::WaylandEventProducer::new() {
        Ok(p) => {
            log::info!("using layer-shell event producer");
            return Box::new(p);
        }
        Err(e) => log::info!("layer_shell event producer not available: {e}"),
    }

    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    match x11::X11Producer::new() {
        Ok(p) => {
            log::info!("using x11 event producer");
            return Box::new(p);
        }
        Err(e) => log::info!("x11 event producer not available: {e}"),
    }

    log::error!("falling back to dummy event producer");
    Box::new(dummy::DummyProducer::new())
}

pub trait EventProducer: Stream<Item = io::Result<(ClientHandle, Event)>> + Unpin {
    /// notify event producer of configuration changes
    fn notify(&mut self, event: ClientEvent) -> io::Result<()>;

    /// release mouse
    fn release(&mut self) -> io::Result<()>;
}
