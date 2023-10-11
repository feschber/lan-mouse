use std::io::Result;
use std::os::fd::{AsRawFd, self};
use std::vec::Drain;

use tokio::io::unix::AsyncFd;

use crate::event::Event;
use crate::producer::EventProducer;

use crate::client::{ClientEvent, ClientHandle};

pub struct X11Producer {
    pending_events: Vec<(ClientHandle, Event)>,
}

impl X11Producer {
    pub fn new() -> Self {
        Self {
            pending_events: vec![],
        }
    }
}

impl AsRawFd for X11Producer {
    fn as_raw_fd(&self) -> fd::RawFd {
        todo!()
    }
}

impl EventProducer for X11Producer {
    fn notify(&mut self, _: ClientEvent) { }

    fn read_events(&mut self) -> Drain<(ClientHandle, Event)> {
        self.pending_events.drain(..)
    }

    fn release(&mut self) {}

    fn get_async_fd(&self) -> Result<AsyncFd<fd::RawFd>> {
        todo!()
    }
}
