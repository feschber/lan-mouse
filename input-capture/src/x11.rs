use std::task::Poll;

use async_trait::async_trait;
use futures_core::Stream;

use super::{Capture, CaptureError, CaptureEvent, Position, error::X11InputCaptureCreationError};

pub struct X11InputCapture {}

impl X11InputCapture {
    pub fn new() -> std::result::Result<Self, X11InputCaptureCreationError> {
        Err(X11InputCaptureCreationError::NotImplemented)
    }
}

#[async_trait]
impl Capture for X11InputCapture {
    async fn create(&mut self, _pos: Position) -> Result<(), CaptureError> {
        Ok(())
    }

    async fn destroy(&mut self, _pos: Position) -> Result<(), CaptureError> {
        Ok(())
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }
}

impl Stream for X11InputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
