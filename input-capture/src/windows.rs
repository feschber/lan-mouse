use async_trait::async_trait;
use core::task::{Context, Poll};
use event_thread::EventThread;
use futures::Stream;
use std::pin::Pin;

use std::task::ready;
use tokio::sync::mpsc::{Receiver, channel};

use super::{Capture, CaptureError, CaptureEvent, Position};

mod display_util;
mod event_thread;

pub struct WindowsInputCapture {
    event_rx: Receiver<(Position, CaptureEvent)>,
    event_thread: EventThread,
}

#[async_trait]
impl Capture for WindowsInputCapture {
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError> {
        self.event_thread.create(pos);
        Ok(())
    }

    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError> {
        self.event_thread.destroy(pos);
        Ok(())
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        self.event_thread.release_capture();
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }
}

impl WindowsInputCapture {
    pub(crate) fn new() -> Self {
        let (event_tx, event_rx) = channel(10);
        let event_thread = EventThread::new(event_tx);
        Self {
            event_thread,
            event_rx,
        }
    }
}

impl Stream for WindowsInputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match ready!(self.event_rx.poll_recv(cx)) {
            None => Poll::Ready(None),
            Some(e) => Poll::Ready(Some(Ok(e))),
        }
    }
}
