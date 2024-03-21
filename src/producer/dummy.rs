use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;

use crate::event::Event;
use crate::producer::EventProducer;

use crate::client::{ClientEvent, ClientHandle};

pub struct DummyProducer {}

impl DummyProducer {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for DummyProducer {
    fn default() -> Self {
        Self::new()
    }
}

impl EventProducer for DummyProducer {
    fn notify(&mut self, _event: ClientEvent) -> io::Result<()> {
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Stream for DummyProducer {
    type Item = io::Result<(ClientHandle, Event)>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
