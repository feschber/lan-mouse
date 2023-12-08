use anyhow::Result;
use std::{io, task::Poll};

use futures_core::Stream;

use crate::{producer::EventProducer, event::Event, client::ClientHandle};

pub struct LibeiProducer {}

impl LibeiProducer {
    pub fn new() -> Result<Self> {
        Ok(Self {  })
    }
}

impl EventProducer for LibeiProducer {
    fn notify(&mut self, _event: crate::client::ClientEvent) {
    }

    fn release(&mut self) {
    }
}

impl Stream for LibeiProducer {
    type Item = io::Result<(ClientHandle, Event)>;

    fn poll_next(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
