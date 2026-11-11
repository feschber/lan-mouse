//! Edge-only X11 input capture.
//!
//! This backend does not grab the pointer or keyboard and never produces
//! input events, so an X11 machine still cannot *send* input with it. What it
//! does is watch the pointer position and report [`CaptureEvent::Begin`] when
//! the pointer arrives at a registered screen edge. That is all a receiving
//! device needs: the service registers an enter-only capture at the edge that
//! faces the sending device, and on `Begin` it tells the sender to release its
//! capture, so the mouse can be moved back across the edge as on other
//! platforms.
//!
//! An edge only fires after the pointer has been seen away from it, so a
//! pointer that already rests on the edge when a device enters does not bounce
//! straight back.

use std::collections::HashSet;
use std::pin::Pin;
use std::ptr;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use async_trait::async_trait;
use futures_core::Stream;
use tokio::time::{self, Interval, MissedTickBehavior};
use x11::xlib::{self, XCloseDisplay};

use super::{Capture, CaptureError, CaptureEvent, Position, error::X11InputCaptureCreationError};

/// How often the pointer position is checked.
const POLL_INTERVAL: Duration = Duration::from_millis(8);

pub struct X11InputCapture {
    display: *mut xlib::Display,
    root: xlib::Window,
    /// edges with an active capture
    positions: HashSet<Position>,
    /// edges the pointer has been away from since they last fired
    armed: HashSet<Position>,
    interval: Interval,
}

// The display connection is only used through `&mut self`, never from two
// threads at once (same reasoning as the X11 emulation backend).
unsafe impl Send for X11InputCapture {}

impl X11InputCapture {
    pub fn new() -> std::result::Result<Self, X11InputCaptureCreationError> {
        let display = unsafe { xlib::XOpenDisplay(ptr::null()) };
        if display.is_null() {
            return Err(X11InputCaptureCreationError::OpenDisplay);
        }
        let root = unsafe { xlib::XDefaultRootWindow(display) };
        let mut interval = time::interval(POLL_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        log::info!(
            "X11 input capture is edge-only: the mouse can be handed back at a screen edge, \
             but this device cannot send input"
        );
        Ok(Self {
            display,
            root,
            positions: HashSet::new(),
            armed: HashSet::new(),
            interval,
        })
    }

    /// Current pointer position and root window size, or `None` if the pointer
    /// is not on this screen or the query fails.
    fn pointer_and_size(&self) -> Option<(i32, i32, i32, i32)> {
        unsafe {
            let (mut root_ret, mut child_ret): (xlib::Window, xlib::Window) = (0, 0);
            let (mut root_x, mut root_y, mut win_x, mut win_y) = (0, 0, 0, 0);
            let mut mask = 0;
            if xlib::XQueryPointer(
                self.display,
                self.root,
                &mut root_ret,
                &mut child_ret,
                &mut root_x,
                &mut root_y,
                &mut win_x,
                &mut win_y,
                &mut mask,
            ) == 0
            {
                return None;
            }

            // queried every time, so a resolution or monitor change is picked up
            let mut geometry_root: xlib::Window = 0;
            let (mut x, mut y) = (0, 0);
            let (mut width, mut height, mut border, mut depth) = (0, 0, 0, 0);
            if xlib::XGetGeometry(
                self.display,
                self.root,
                &mut geometry_root,
                &mut x,
                &mut y,
                &mut width,
                &mut height,
                &mut border,
                &mut depth,
            ) == 0
            {
                return None;
            }
            Some((root_x, root_y, width as i32, height as i32))
        }
    }

    /// Returns an edge the pointer has just arrived at, if any.
    fn check_edges(&mut self) -> Option<Position> {
        if self.positions.is_empty() {
            return None;
        }
        let (x, y, width, height) = self.pointer_and_size()?;
        let mut hit = None;
        for &pos in self.positions.iter() {
            let at_edge = match pos {
                Position::Left => x <= 0,
                Position::Right => x >= width - 1,
                Position::Top => y <= 0,
                Position::Bottom => y >= height - 1,
            };
            if !at_edge {
                self.armed.insert(pos);
            } else if hit.is_none() && self.armed.remove(&pos) {
                hit = Some(pos);
            }
        }
        hit
    }
}

impl Drop for X11InputCapture {
    fn drop(&mut self) {
        unsafe {
            XCloseDisplay(self.display);
        }
    }
}

#[async_trait]
impl Capture for X11InputCapture {
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError> {
        log::debug!("X11 edge capture: watching {pos} edge");
        // armed on the next check that finds the pointer away from this edge
        self.positions.insert(pos);
        Ok(())
    }

    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError> {
        log::debug!("X11 edge capture: no longer watching {pos} edge");
        self.positions.remove(&pos);
        self.armed.remove(&pos);
        Ok(())
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        // nothing is grabbed, so there is nothing to release
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        self.positions.clear();
        self.armed.clear();
        Ok(())
    }
}

impl Stream for X11InputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            ready!(self.interval.poll_tick(cx));
            if let Some(pos) = self.check_edges() {
                log::debug!("X11 edge capture: pointer reached the {pos} edge");
                return Poll::Ready(Some(Ok((pos, CaptureEvent::Begin))));
            }
        }
    }
}
