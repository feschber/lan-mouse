use std::vec::Drain;

use mio::{Token, Registry};
use mio::event::Source;
use std::io::Result;

use crate::{
    client::{ClientHandle, ClientEvent},
    event::Event,
    producer::EventProducer,
};

pub struct WindowsProducer {
    pending_events: Vec<(ClientHandle, Event)>,
}

impl Source for WindowsProducer {
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


impl EventProducer for WindowsProducer {
    fn notify(&mut self, _: ClientEvent) { }

    fn read_events(&mut self) -> Drain<(ClientHandle, Event)> {
        self.pending_events.drain(..)
    }

    fn release(&mut self) { }
}

impl WindowsProducer {
    pub(crate) fn new() -> Self {
        Self {
            pending_events: vec![],
        }
    }
}
