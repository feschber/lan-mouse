use std::io;

use futures_core::Stream;

use crate::backend::producer;
use crate::{
    client::{ClientEvent, ClientHandle},
    event::Event,
};

pub async fn create() -> Box<dyn EventProducer> {
    #[cfg(target_os = "macos")]
    match producer::macos::MacOSProducer::new() {
        Ok(p) => return Box::new(p),
        Err(e) => log::info!("macos event producer not available: {e}"),
    }

    #[cfg(windows)]
    match producer::windows::WindowsProducer::new() {
        Ok(p) => return Box::new(p),
        Err(e) => log::info!("windows event producer not available: {e}"),
    }

    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    match producer::libei::LibeiProducer::new() {
        Ok(p) => {
            log::info!("using libei event producer");
            return Box::new(p);
        }
        Err(e) => log::info!("libei event producer not available: {e}"),
    }

    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    match producer::wayland::WaylandEventProducer::new() {
        Ok(p) => {
            log::info!("using layer-shell event producer");
            return Box::new(p);
        }
        Err(e) => log::info!("layer_shell event producer not available: {e}"),
    }

    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    match producer::x11::X11Producer::new() {
        Ok(p) => {
            log::info!("using x11 event producer");
            return Box::new(p);
        }
        Err(e) => log::info!("x11 event producer not available: {e}"),
    }

    log::error!("falling back to dummy event producer");
    Box::new(producer::dummy::DummyProducer::new())
}

pub trait EventProducer: Stream<Item = io::Result<(ClientHandle, Event)>> + Unpin {
    /// notify event producer of configuration changes
    fn notify(&mut self, event: ClientEvent) -> io::Result<()>;

    /// release mouse
    fn release(&mut self) -> io::Result<()>;
}
