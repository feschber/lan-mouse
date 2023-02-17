#[cfg(unix)]
use std::env;
use std::{thread::{JoinHandle, self}, sync::mpsc::SyncSender};

use crate::{client::{Client, ClientHandle}, event::Event, request::Server};

use crate::backend::producer;

#[cfg(unix)]
enum Backend {
    Wayland,
    X11,
}

pub fn start(
    produce_tx: SyncSender<(Event, ClientHandle)>,
    clients: Vec<Client>,
    request_server: Server,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("event producer".into())
        .spawn(move || {
            #[cfg(windows)]
            producer::windows::run(produce_tx, request_server, clients);

            #[cfg(unix)]
            let backend = match env::var("XDG_SESSION_TYPE") {
                Ok(session_type) => match session_type.as_str() {
                    "x11" => Backend::X11,
                    "wayland" => Backend::Wayland,
                    _ => panic!("unknown XDG_SESSION_TYPE"),
                },
                Err(_) => panic!("could not detect session type: XDG_SESSION_TYPE environment variable not set!"),
            };

            #[cfg(unix)]
            match backend {
                Backend::X11 => {
                    #[cfg(not(feature = "x11"))]
                    panic!("feature x11 not enabled");
                    #[cfg(feature = "x11")]
                    producer::x11::run(produce_tx, request_server, clients);
                }
                Backend::Wayland => {
                    #[cfg(not(feature = "wayland"))]
                    panic!("feature wayland not enabled");
                    #[cfg(feature = "wayland")]
                    producer::wayland::run(produce_tx, request_server, clients);
                }
            }
        })
        .unwrap()
}
