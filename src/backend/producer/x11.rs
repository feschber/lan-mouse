use std::{vec::Drain, os::fd::{RawFd, AsRawFd}};

use crate::{producer::EpollProducer, client::{ClientHandle, ClientEvent}, event::Event};

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

impl EpollProducer for X11Producer {
    fn notify(&mut self, _: ClientEvent) {}

    fn eventfd(&self) -> RawFd {
        1.as_raw_fd()
    }

    fn read_events(&mut self) -> Drain<(ClientHandle, Event)> {
        self.pending_events.drain(..)
    }

    fn release(&mut self) {}
}
