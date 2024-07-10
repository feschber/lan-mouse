use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures_core::Stream;

use input_event::Event;

use crate::CaptureError;

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

#[async_trait]
impl<'a> InputCapture for DummyInputCapture {
    async fn create(&mut self, _handle: CaptureHandle, _pos: Position) -> io::Result<()> {
        Ok(())
    }

    async fn destroy(&mut self, _handle: CaptureHandle) -> io::Result<()> {
        Ok(())
    }

    async fn release(&mut self) -> io::Result<()> {
        Ok(())
    }

    async fn async_drop(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }
}

impl Stream for DummyInputCapture {
    type Item = Result<(CaptureHandle, Event), CaptureError>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
