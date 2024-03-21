use crate::capture::InputCapture;
use crate::client::{ClientEvent, ClientHandle};
use crate::event::Event;
use anyhow::{anyhow, Result};
use futures_core::Stream;
use std::task::{Context, Poll};
use std::{io, pin::Pin};

pub struct MacOSInputCapture;

impl MacOSInputCapture {
    pub fn new() -> Result<Self> {
        Err(anyhow!("not yet implemented"))
    }
}

impl Stream for MacOSInputCapture {
    type Item = io::Result<(ClientHandle, Event)>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

impl InputCapture for MacOSInputCapture {
    fn notify(&mut self, _event: ClientEvent) -> io::Result<()> {
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        Ok(())
    }
}
