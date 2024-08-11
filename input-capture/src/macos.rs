use crate::{
    error::MacOSInputCaptureCreationError, Capture, CaptureError, CaptureEvent, CaptureHandle,
    Position,
};
use async_trait::async_trait;
use futures_core::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

pub struct MacOSInputCapture;

impl MacOSInputCapture {
    pub fn new() -> std::result::Result<Self, MacOSInputCaptureCreationError> {
        Err(MacOSInputCaptureCreationError::NotImplemented)
    }
}

impl Stream for MacOSInputCapture {
    type Item = Result<(CaptureHandle, CaptureEvent), CaptureError>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

#[async_trait]
impl Capture for MacOSInputCapture {
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
