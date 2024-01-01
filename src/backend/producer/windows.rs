use anyhow::{anyhow, Result};
use core::task::{Context, Poll};
use futures::Stream;
use std::{io, pin::Pin};

use crate::{
    client::{ClientEvent, ClientHandle},
    event::Event,
    producer::EventProducer,
};

pub struct WindowsProducer {}

impl EventProducer for WindowsProducer {
    fn notify(&mut self, _event: ClientEvent) -> io::Result<()> {
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl WindowsProducer {
    pub(crate) fn new() -> Result<Self> {
        Err(anyhow!("not implemented"))
    }
}

impl Stream for WindowsProducer {
    type Item = io::Result<(ClientHandle, Event)>;
    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
