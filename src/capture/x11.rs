use std::io;
use std::task::Poll;

use futures_core::Stream;

use super::InputCapture;
use crate::event::Event;

use super::error::X11InputCaptureCreationError;
use super::{CaptureHandle, Position};

pub struct X11InputCapture {}

impl X11InputCapture {
    pub fn new() -> std::result::Result<Self, X11InputCaptureCreationError> {
        Err(X11InputCaptureCreationError::NotImplemented)
    }
}

impl InputCapture for X11InputCapture {
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

impl Stream for X11InputCapture {
    type Item = io::Result<(CaptureHandle, Event)>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
