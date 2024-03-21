use anyhow::{anyhow, Result};
use core::task::{Context, Poll};
use futures::Stream;
use std::{io, pin::Pin};

use crate::{
    capture::InputCapture,
    client::{ClientEvent, ClientHandle},
    event::Event,
};

pub struct WindowsInputCapture {}

impl InputCapture for WindowsInputCapture {
    fn notify(&mut self, _event: ClientEvent) -> io::Result<()> {
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl WindowsInputCapture {
    pub(crate) fn new() -> Result<Self> {
        Err(anyhow!("not implemented"))
    }
}

impl Stream for WindowsInputCapture {
    type Item = io::Result<(ClientHandle, Event)>;
    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
