use crate::client::{ClientEvent, ClientHandle};
use crate::event::Event;
use crate::producer::EventProducer;
use futures_core::Stream;
use std::task::{Context, Poll};
use std::{io, pin::Pin};

pub struct MacOSProducer;

impl MacOSProducer {
    pub fn new() -> Self {
        Self {}
    }
}

impl Stream for MacOSProducer {
    type Item = io::Result<(ClientHandle, Event)>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

impl EventProducer for MacOSProducer {
    fn notify(&mut self, _event: ClientEvent) {}

    fn release(&mut self) {}
}
