#[cfg(unix)]
use std::env;
use std::{sync::mpsc::Receiver, error::Error, os::fd::RawFd, vec::Drain};

use crate::{client::{ClientHandle, ClientEvent}, event::Event};

use crate::backend::producer;

#[cfg(unix)]
enum Backend {
    Wayland,
    X11,
}

pub fn create() -> Result<EventProducer, Box<dyn Error>> {
    #[cfg(windows)]
    producer::windows::run(produce_tx, request_server, clients);

    #[cfg(unix)]
    let backend = match env::var("XDG_SESSION_TYPE") {
        Ok(session_type) => match session_type.as_str() {
            "x11" => {
                log::info!("XDG_SESSION_TYPE = x11 -> using X11 event producer");
                Backend::X11
            },
            "wayland" => {
                log::info!("XDG_SESSION_TYPE = wayland -> using wayland event producer");
                Backend::Wayland
            }
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
            Ok(EventProducer::Epoll(Box::new(producer::x11::X11Producer::new())))
        }
        Backend::Wayland => {
            #[cfg(not(feature = "wayland"))]
            panic!("feature wayland not enabled");
            #[cfg(feature = "wayland")]
            Ok(EventProducer::Epoll(Box::new(producer::wayland::WaylandEventProducer::new()?)))
        }
    }
}

pub trait EpollProducer {
    /// notify event producer of configuration changes
    fn notify(&mut self, event: ClientEvent);

    /// handle to the eventfd for a producer
    fn eventfd(&self) -> RawFd;

    /// read an event
    /// this function must be invoked to retrieve an Event after
    /// the eventfd indicates a pending Event
    fn read_events(&mut self) -> Drain<(ClientHandle, Event)>;

    /// release mouse
    fn release(&mut self);
}

pub trait ThreadProducer {
    /// notify event producer of configuration changes
    fn notify(&self, event: ClientEvent);

    /// get the recieving end of the event channel to read from
    fn wait_channel(&self) -> Receiver<(ClientHandle, Event)>;

    /// stop the producer thread
    fn stop(&self);
}

pub enum EventProducer {
    /// epoll based event producer
    Epoll(Box<dyn EpollProducer>),

    /// mpsc channel based event producer
    ThreadProducer(Box<dyn ThreadProducer>),
}
