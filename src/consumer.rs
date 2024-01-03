use async_trait::async_trait;
use std::future;

use crate::{
    backend::consumer,
    client::{ClientEvent, ClientHandle},
    event::Event,
};
use anyhow::Result;

#[async_trait]
pub trait EventConsumer: Send {
    async fn consume(&mut self, event: Event, client_handle: ClientHandle);
    async fn notify(&mut self, client_event: ClientEvent);
    /// this function is waited on continuously and can be used to handle events
    async fn dispatch(&mut self) -> Result<()> {
        let _: () = future::pending().await;
        Ok(())
    }

    async fn destroy(&mut self);
}

pub async fn create() -> Box<dyn EventConsumer> {
    #[cfg(windows)]
    match consumer::windows::WindowsConsumer::new() {
        Ok(c) => return Box::new(c),
        Err(e) => log::warn!("windows event consumer unavailable: {e}"),
    }

    #[cfg(target_os = "macos")]
    match consumer::macos::MacOSConsumer::new() {
        Ok(c) => {
            log::info!("using macos event consumer");
            return Box::new(c);
        }
        Err(e) => log::error!("macos consumer not available: {e}"),
    }

    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    match consumer::wlroots::WlrootsConsumer::new() {
        Ok(c) => {
            log::info!("using wlroots event consumer");
            return Box::new(c);
        }
        Err(e) => log::info!("wayland backend not available: {e}"),
    }

    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    match consumer::libei::LibeiConsumer::new().await {
        Ok(c) => {
            log::info!("using libei event consumer");
            return Box::new(c);
        }
        Err(e) => log::info!("libei not available: {e}"),
    }

    #[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
    match consumer::xdg_desktop_portal::DesktopPortalConsumer::new().await {
        Ok(c) => {
            log::info!("using xdg-remote-desktop-portal event consumer");
            return Box::new(c);
        }
        Err(e) => log::info!("remote desktop portal not available: {e}"),
    }

    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    match consumer::x11::X11Consumer::new() {
        Ok(c) => {
            log::info!("using x11 event consumer");
            return Box::new(c);
        }
        Err(e) => log::info!("x11 consumer not available: {e}"),
    }

    log::error!("falling back to dummy event consumer");
    Box::new(consumer::dummy::DummyConsumer::new())
}
