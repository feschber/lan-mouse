#[cfg(unix)]
use std::env;

use anyhow::Result;
use crate::{backend::consumer, client::{ClientHandle, ClientEvent}, event::Event};

#[cfg(unix)]
#[derive(Debug)]
enum Backend {
    Wlroots,
    X11,
    RemoteDesktopPortal,
    Libei,
}

pub trait Consumer {
    /// Event corresponding to an abstract `client_handle`
    fn consume(&self, event: Event, client_handle: ClientHandle);

    /// Event corresponding to a configuration change
    fn notify(&mut self, client_event: ClientEvent);
}

pub fn create() -> Result<Box<dyn Consumer>> {
    #[cfg(windows)]
    let _backend = backend;

    #[cfg(windows)]
    consumer::windows::run(consume_rx, clients);

    #[cfg(unix)]
    let backend = match env::var("XDG_SESSION_TYPE") {
        Ok(session_type) => match session_type.as_str() {
            "x11" => {
                log::info!("XDG_SESSION_TYPE = x11 -> using x11 event consumer");
                Backend::X11
            }
            "wayland" => {
                log::info!("XDG_SESSION_TYPE = wayland -> using wayland event consumer");
                match env::var("XDG_CURRENT_DESKTOP") {
                    Ok(current_desktop) => match current_desktop.as_str() {
                        "gnome" => {
                            log::info!("XDG_CURRENT_DESKTOP = kde -> using libei backend");
                            Backend::Libei
                        }
                        "kde" => {
                            log::info!("XDG_CURRENT_DESKTOP = kde -> using xdg_desktop_portal backend");
                            Backend::RemoteDesktopPortal
                        }
                        "sway" => {
                            log::info!("XDG_CURRENT_DESKTOP = sway -> using wlroots backend");
                            Backend::Wlroots
                        }
                        "Hyprland" => {
                            log::info!("XDG_CURRENT_DESKTOP = Hyprland -> using wlroots backend");
                            Backend::Wlroots
                        }
                        _ => {
                            log::warn!("unknown XDG_CURRENT_DESKTOP -> defaulting to wlroots backend");
                            Backend::Wlroots
                        }
                    }
                    // default to wlroots backend for now
                    _ => {
                        log::warn!("unknown XDG_CURRENT_DESKTOP -> defaulting to wlroots backend");
                        Backend::Wlroots
                    }
                }
            }
            _ => panic!("unknown XDG_SESSION_TYPE"),
        },
        Err(_) => panic!("could not detect session type: XDG_SESSION_TYPE environment variable not set!"),
    };

    #[cfg(unix)]
    match backend {
        Backend::Libei => {
            #[cfg(not(feature = "libei"))]
            panic!("feature libei not enabled");
            #[cfg(feature = "libei")]
            Ok(Box::new(consumer::libei::LibeiConsumer::new()))
        },
        Backend::RemoteDesktopPortal => {
            #[cfg(not(feature = "xdg_desktop_portal"))]
            panic!("feature xdg_desktop_portal not enabled");
            #[cfg(feature = "xdg_desktop_portal")]
            Ok(Box::new(consumer::xdg_desktop_portal::DesktopPortalConsumer::new()))
        },
        Backend::Wlroots => {
            #[cfg(not(feature = "wayland"))]
            panic!("feature wayland not enabled");
            #[cfg(feature = "wayland")]
            Ok(Box::new(consumer::wlroots::WlrootsConsumer::new()?))
        },
        Backend::X11 => {
            #[cfg(not(feature = "x11"))]
            panic!("feature x11 not enabled");
            #[cfg(feature = "x11")]
            Ok(Box::new(consumer::x11::X11Consumer::new()))
        },
    }
}
