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

/// fallback input emulation (logs events)
pub mod dummy;

#[async_trait]
pub trait InputEmulation: Send {
    async fn consume(&mut self, event: Event, client_handle: ClientHandle);
    async fn notify(&mut self, client_event: ClientEvent);
    /// this function is waited on continuously and can be used to handle events
    async fn dispatch(&mut self) -> Result<()> {
        let _: () = future::pending().await;
        Ok(())
    }

    async fn destroy(&mut self);
}

pub async fn create() -> Box<dyn InputEmulation> {
    #[cfg(windows)]
    match windows::WindowsEmulation::new() {
        Ok(c) => return Box::new(c),
        Err(e) => log::warn!("windows input emulation unavailable: {e}"),
    }

    #[cfg(target_os = "macos")]
    match macos::MacOSEmulation::new() {
        Ok(c) => {
            log::info!("using macos input emulation");
            return Box::new(c);
        }
        Err(e) => log::error!("macos input emulatino not available: {e}"),
    }

    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    match wlroots::WlrootsEmulation::new() {
        Ok(c) => {
            log::info!("using wlroots input emulation");
            return Box::new(c);
        }
        Err(e) => log::info!("wayland backend not available: {e}"),
    }

    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    match libei::LibeiEmulation::new().await {
        Ok(c) => {
            log::info!("using libei input emulation");
            return Box::new(c);
        }
        Err(e) => log::info!("libei not available: {e}"),
    }

    #[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
    match xdg_desktop_portal::DesktopPortalEmulation::new().await {
        Ok(c) => {
            log::info!("using xdg-remote-desktop-portal input emulation");
            return Box::new(c);
        }
        Err(e) => log::info!("remote desktop portal not available: {e}"),
    }

    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    match x11::X11Emulation::new() {
        Ok(c) => {
            log::info!("using x11 input emulation");
            return Box::new(c);
        }
        Err(e) => log::info!("x11 input emulation not available: {e}"),
    }

    log::error!("falling back to dummy input emulation");
    Box::new(dummy::DummyEmulation::new())
}
