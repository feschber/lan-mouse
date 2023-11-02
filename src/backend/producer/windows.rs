use std::io::Result;
use std::pin::Pin;
use futures::Stream;
use core::task::{Context, Poll};

use crate::{
    client::{ClientHandle, ClientEvent},
    event::Event,
    producer::EventProducer,
};

pub struct WindowsProducer { }

impl EventProducer for WindowsProducer {
    fn notify(&mut self, _: ClientEvent) { }

    fn release(&mut self) { }
}

impl WindowsProducer {
    pub(crate) fn new() -> Self {
        Self {}
    }
}

impl Stream for WindowsProducer {
    type Item = Result<(ClientHandle, Event)>;
    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
