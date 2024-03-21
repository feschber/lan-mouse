use async_trait::async_trait;
use std::future;

use crate::{
    client::{ClientEvent, ClientHandle},
    event::Event,
};
use anyhow::Result;

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

/// fallback consumer
pub mod dummy;

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
    match windows::WindowsConsumer::new() {
        Ok(c) => return Box::new(c),
        Err(e) => log::warn!("windows event consumer unavailable: {e}"),
    }

    #[cfg(target_os = "macos")]
    match macos::MacOSConsumer::new() {
        Ok(c) => {
            log::info!("using macos event consumer");
            return Box::new(c);
        }
        Err(e) => log::error!("macos consumer not available: {e}"),
    }

    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    match wlroots::WlrootsConsumer::new() {
        Ok(c) => {
            log::info!("using wlroots event consumer");
            return Box::new(c);
        }
        Err(e) => log::info!("wayland backend not available: {e}"),
    }

    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    match libei::LibeiConsumer::new().await {
        Ok(c) => {
            log::info!("using libei event consumer");
            return Box::new(c);
        }
        Err(e) => log::info!("libei not available: {e}"),
    }

    #[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
    match xdg_desktop_portal::DesktopPortalConsumer::new().await {
        Ok(c) => {
            log::info!("using xdg-remote-desktop-portal event consumer");
            return Box::new(c);
        }
        Err(e) => log::info!("remote desktop portal not available: {e}"),
    }

    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    match x11::X11Consumer::new() {
        Ok(c) => {
            log::info!("using x11 event consumer");
            return Box::new(c);
        }
        Err(e) => log::info!("x11 consumer not available: {e}"),
    }

    log::error!("falling back to dummy event consumer");
    Box::new(dummy::DummyConsumer::new())
}
