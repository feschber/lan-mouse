use std::vec::Drain;

use mio::{Token, Registry};
use mio::event::Source;
use std::io::Result;

use crate::producer::EventProducer;

use crate::{client::{ClientHandle, ClientEvent}, event::Event};

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

impl Source for X11Producer {
    fn register(
        &mut self,
        _registry: &Registry,
        _token: Token,
        _interests: mio::Interest,
    ) -> Result<()> {
        Ok(())
    }

    fn reregister(
        &mut self,
        _registry: &Registry,
        _token: Token,
        _interests: mio::Interest,
    ) -> Result<()> {
        Ok(())
    }

    fn deregister(&mut self, _registry: &Registry) -> Result<()> {
        Ok(())
    }
}

impl EventProducer for X11Producer {
    fn notify(&mut self, _: ClientEvent) { }

    fn read_events(&mut self) -> Drain<(ClientHandle, Event)> {
        self.pending_events.drain(..)
    }

    fn release(&mut self) {}
}
