use std::task::Poll;

use async_trait::async_trait;
use futures_core::Stream;

use crate::CaptureError;

use super::Capture;
use input_event::Event;

use super::error::X11InputCaptureCreationError;
use super::{CaptureHandle, Position};

pub struct X11InputCapture {}

impl X11InputCapture {
    pub fn new() -> std::result::Result<Self, X11InputCaptureCreationError> {
        Err(X11InputCaptureCreationError::NotImplemented)
    }
}

#[async_trait]
impl Capture for X11InputCapture {
    async fn create(&mut self, _id: CaptureHandle, _pos: Position) -> Result<(), CaptureError> {
        Ok(())
    }

    async fn destroy(&mut self, _id: CaptureHandle) -> Result<(), CaptureError> {
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
    type Item = Result<(CaptureHandle, Event), CaptureError>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
