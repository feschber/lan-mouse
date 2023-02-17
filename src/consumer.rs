use std::{thread::{JoinHandle, self}, sync::mpsc::Receiver};

#[cfg(unix)]
use std::env;

use crate::{backend::consumer, client::{Client, ClientHandle}, event::Event};

#[cfg(unix)]
#[derive(Debug)]
enum Backend {
    Wlroots,
    X11,
    RemoteDesktopPortal,
    Libei,
}

pub fn start(consume_rx: Receiver<(Event, ClientHandle)>, clients: Vec<Client>, backend: Option<String>) -> JoinHandle<()> {
    #[cfg(windows)]
    let _backend = backend;

    thread::Builder::new()
        .name("event consumer".into())
        .spawn(move || {
            #[cfg(windows)]
            consumer::windows::run(consume_rx, clients);

            #[cfg(unix)]
            let backend = match env::var("XDG_SESSION_TYPE") {
                Ok(session_type) => match session_type.as_str() {
                    "x11" => Backend::X11,
                    "wayland" => {
                        match backend {
                            Some(backend) => match backend.as_str() {
                                "wlroots" => Backend::Wlroots,
                                "libei" => Backend::Libei,
                                "xdg_desktop_portal" => Backend::RemoteDesktopPortal,
                                backend => panic!("invalid backend: {}", backend)
                            }
                            // default to wlroots backend for now
                            _ => Backend::Wlroots,
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
                    consumer::libei::run(consume_rx, clients);
                },
                Backend::RemoteDesktopPortal => {
                    #[cfg(not(feature = "xdg_desktop_portal"))]
                    panic!("feature xdg_desktop_portal not enabled");
                    #[cfg(feature = "xdg_desktop_portal")]
                    consumer::xdg_desktop_portal::run(consume_rx, clients);
                },
                Backend::Wlroots => {
                    #[cfg(not(feature = "wayland"))]
                    panic!("feature wayland not enabled");
                    #[cfg(feature = "wayland")]
                    consumer::wlroots::run(consume_rx, clients);
                },
                Backend::X11 => {
                    #[cfg(not(feature = "x11"))]
                    panic!("feature x11 not enabled");
                    #[cfg(feature = "x11")]
                    consumer::x11::run(consume_rx, clients);
                },
            }
        })
        .unwrap()
}
