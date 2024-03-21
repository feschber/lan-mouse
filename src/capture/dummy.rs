use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;

use crate::capture::InputCapture;
use crate::event::Event;

use crate::client::{ClientEvent, ClientHandle};

pub struct DummyInputCapture {}

impl DummyInputCapture {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for DummyInputCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl InputCapture for DummyInputCapture {
    fn notify(&mut self, _event: ClientEvent) -> io::Result<()> {
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Stream for DummyInputCapture {
    type Item = io::Result<(ClientHandle, Event)>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
