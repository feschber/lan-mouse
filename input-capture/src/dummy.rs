use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;

use input_event::Event;

use super::{CaptureHandle, InputCapture, Position};

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
    fn create(&mut self, _handle: CaptureHandle, _pos: Position) -> io::Result<()> {
        Ok(())
    }

    fn destroy(&mut self, _handle: CaptureHandle) -> io::Result<()> {
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Stream for DummyInputCapture {
    type Item = io::Result<(CaptureHandle, Event)>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
