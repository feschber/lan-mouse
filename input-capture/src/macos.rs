use crate::{
    error::MacOSInputCaptureCreationError, CaptureError, CaptureHandle, InputCapture, Position,
};
use async_trait::async_trait;
use futures_core::Stream;
use input_event::Event;
use std::task::{Context, Poll};
use std::{io, pin::Pin};

pub struct MacOSInputCapture;

impl MacOSInputCapture {
    pub fn new() -> std::result::Result<Self, MacOSInputCaptureCreationError> {
        Err(MacOSInputCaptureCreationError::NotImplemented)
    }
}

impl Stream for MacOSInputCapture {
    type Item = Result<(CaptureHandle, Event), CaptureError>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

#[async_trait]
impl InputCapture for MacOSInputCapture {
    async fn create(&mut self, _id: CaptureHandle, _pos: Position) -> io::Result<()> {
        Ok(())
    }

    async fn destroy(&mut self, _id: CaptureHandle) -> io::Result<()> {
        Ok(())
    }

    async fn release(&mut self) -> io::Result<()> {
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }
}
