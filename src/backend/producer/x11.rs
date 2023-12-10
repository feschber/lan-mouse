use std::io;
use std::task::Poll;

use futures_core::Stream;

use crate::event::Event;
use crate::producer::EventProducer;

use crate::client::{ClientEvent, ClientHandle};

pub struct X11Producer {}

impl X11Producer {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for X11Producer {
    fn default() -> Self {
        Self::new()
    }
}

impl EventProducer for X11Producer {
    fn notify(&mut self, _: ClientEvent) {}

    fn release(&mut self) {}
}

impl Stream for X11Producer {
    type Item = io::Result<(ClientHandle, Event)>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
