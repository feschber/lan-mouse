use crate::capture::error::MacOSInputCaptureCreationError;
use crate::capture::{CaptureHandle, InputCapture, Position};
use crate::event::Event;
use futures_core::Stream;
use std::task::{Context, Poll};
use std::{io, pin::Pin};

pub struct MacOSInputCapture;

impl MacOSInputCapture {
    pub fn new() -> std::result::Result<Self, MacOSInputCaptureCreationError> {
        Err(MacOSInputCaptureCreationError::NotImplemented)
    }
}

impl Stream for MacOSInputCapture {
    type Item = io::Result<(CaptureHandle, Event)>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}

impl InputCapture for MacOSInputCapture {
    fn create(&mut self, _id: CaptureHandle, _pos: Position) -> io::Result<()> {
        Ok(())
    }

    fn destroy(&mut self, _id: CaptureHandle) -> io::Result<()> {
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        Ok(())
    }
}
