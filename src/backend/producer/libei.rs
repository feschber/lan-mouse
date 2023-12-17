use anyhow::{anyhow, Result};
use std::{io, task::Poll};

use futures_core::Stream;

use crate::{client::ClientHandle, event::Event, producer::EventProducer};

pub struct LibeiProducer {}

impl LibeiProducer {
    pub fn new() -> Result<Self> {
        Err(anyhow!("not implemented"))
    }
}

impl EventProducer for LibeiProducer {
    fn notify(&mut self, _event: crate::client::ClientEvent) {}

    fn release(&mut self) {}
}

impl Stream for LibeiProducer {
    type Item = io::Result<(ClientHandle, Event)>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
