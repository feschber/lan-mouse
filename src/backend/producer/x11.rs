use std::vec::Drain;

use crate::producer::EpollProducer;

pub struct X11Producer {}

impl X11Producer {
    pub fn new() -> Self {
        todo!()
    }
}

impl EpollProducer for X11Producer {
    fn notify(&mut self, _: crate::client::ClientEvent) {
        todo!()
    }

    fn eventfd(&self) -> std::os::fd::RawFd {
        todo!()
    }

    fn read_events(&mut self) -> Drain<(crate::client::ClientHandle, crate::event::Event)> {
        todo!()
    }
}
