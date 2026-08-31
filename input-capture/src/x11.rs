use std::collections::HashSet;
use std::pin::Pin;
use std::sync::mpsc;
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;

use async_trait::async_trait;
use futures_core::Stream;
use tokio::sync::mpsc as tokio_mpsc;

use x11::xlib::{
    ButtonMotionMask, ButtonPress, ButtonPressMask, ButtonRelease, ButtonReleaseMask, CurrentTime,
    Display, False, GrabModeAsync, GrabSuccess, KeyPress, KeyRelease, MotionNotify,
    PointerMotionMask, Window, XButtonEvent, XCloseDisplay, XDefaultRootWindow, XDefaultScreen,
    XDisplayHeight, XDisplayWidth, XEvent, XFlush, XGrabKeyboard, XGrabPointer, XKeyEvent,
    XMotionEvent, XNextEvent, XPending, XQueryPointer, XUngrabKeyboard, XUngrabPointer,
    XWarpPointer,
};

use input_event::{Event, KeyboardEvent, PointerEvent};

use super::{error::X11InputCaptureCreationError, Capture, CaptureError, CaptureEvent, Position};

// ── Request enum (async → thread) ────────────────────────────────────────────

enum Request {
    Create(Position),
    Destroy(Position),
    Release,
    Terminate,
}

// ── Internal thread state ─────────────────────────────────────────────────────

struct X11State {
    display: *mut Display,
    root: Window,
    screen_w: i32,
    screen_h: i32,
    clients: HashSet<Position>,
    active_client: Option<Position>,
    entry_point: (i32, i32),
    prev_pos: (i32, i32),
    event_tx: tokio_mpsc::Sender<(Position, CaptureEvent)>,
    request_rx: mpsc::Receiver<Request>,
}

// Safety: display is only accessed from the dedicated X11 thread.
unsafe impl Send for X11State {}

// ── Public struct ─────────────────────────────────────────────────────────────

pub struct X11InputCapture {
    event_rx: tokio_mpsc::Receiver<(Position, CaptureEvent)>,
    request_tx: mpsc::SyncSender<Request>,
    thread: Option<thread::JoinHandle<()>>,
}

impl X11InputCapture {
    pub fn new() -> Result<Self, X11InputCaptureCreationError> {
        let display = unsafe { x11::xlib::XOpenDisplay(std::ptr::null()) };
        if display.is_null() {
            return Err(X11InputCaptureCreationError::OpenDisplayFailed);
        }

        let screen = unsafe { XDefaultScreen(display) };
        let root = unsafe { XDefaultRootWindow(display) };
        let screen_w = unsafe { XDisplayWidth(display, screen) };
        let screen_h = unsafe { XDisplayHeight(display, screen) };

        let (event_tx, event_rx) = tokio_mpsc::channel(64);
        let (request_tx, request_rx) = mpsc::sync_channel(16);
        let (ready_tx, ready_rx) = mpsc::channel::<()>();

        let state = X11State {
            display,
            root,
            screen_w,
            screen_h,
            clients: HashSet::new(),
            active_client: None,
            entry_point: (0, 0),
            prev_pos: (0, 0),
            event_tx,
            request_rx,
        };

        let thread = thread::spawn(move || {
            ready_tx.send(()).expect("ready channel closed");
            run_event_loop(state);
        });

        ready_rx.recv().expect("ready channel closed");

        Ok(Self {
            event_rx,
            request_tx,
            thread: Some(thread),
        })
    }
}

impl Drop for X11InputCapture {
    fn drop(&mut self) {
        let _ = self.request_tx.send(Request::Terminate);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ── Async trait impl ──────────────────────────────────────────────────────────

#[async_trait]
impl Capture for X11InputCapture {
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError> {
        let _ = self.request_tx.send(Request::Create(pos));
        Ok(())
    }

    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError> {
        let _ = self.request_tx.send(Request::Destroy(pos));
        Ok(())
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        let _ = self.request_tx.send(Request::Release);
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        let _ = self.request_tx.send(Request::Terminate);
        Ok(())
    }
}

impl Stream for X11InputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.event_rx.poll_recv(cx) {
            Poll::Ready(Some(e)) => Poll::Ready(Some(Ok(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

// ── Event loop ────────────────────────────────────────────────────────────────

fn run_event_loop(mut state: X11State) {
    loop {
        if drain_requests(&mut state) {
            return;
        }

        if state.active_client.is_none() {
            // Phase 1: poll cursor position, detect edge crossing
            let curr = query_pointer(&state);
            if let Some(pos) =
                crossed_boundary(state.prev_pos, curr, state.screen_w, state.screen_h)
            {
                if state.clients.contains(&pos) {
                    let entry = clamp_to_screen(curr, state.screen_w, state.screen_h);
                    do_grab(&mut state, pos, entry);
                }
            }
            state.prev_pos = curr;
            thread::sleep(Duration::from_millis(1));
        } else {
            // Phase 2: drain X11 event queue (populated by XGrabPointer)
            let pending = unsafe { XPending(state.display) };
            if pending > 0 {
                let mut ev = unsafe { std::mem::zeroed::<XEvent>() };
                unsafe { XNextEvent(state.display, &mut ev) };
                handle_event(&mut state, ev);
            } else {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

/// Drains pending requests. Returns `true` if the thread should terminate.
fn drain_requests(state: &mut X11State) -> bool {
    loop {
        match state.request_rx.try_recv() {
            Ok(Request::Create(pos)) => {
                state.clients.insert(pos);
            }
            Ok(Request::Destroy(pos)) => {
                state.clients.remove(&pos);
                if state.active_client == Some(pos) {
                    do_release(state);
                }
            }
            Ok(Request::Release) => do_release(state),
            Ok(Request::Terminate) => {
                unsafe { XCloseDisplay(state.display) };
                return true;
            }
            Err(_) => return false,
        }
    }
}

fn query_pointer(state: &X11State) -> (i32, i32) {
    let mut root_return: Window = 0;
    let mut child_return: Window = 0;
    let mut root_x: i32 = 0;
    let mut root_y: i32 = 0;
    let mut win_x: i32 = 0;
    let mut win_y: i32 = 0;
    let mut mask: u32 = 0;
    unsafe {
        XQueryPointer(
            state.display,
            state.root,
            &mut root_return,
            &mut child_return,
            &mut root_x,
            &mut root_y,
            &mut win_x,
            &mut win_y,
            &mut mask,
        )
    };
    (root_x, root_y)
}

fn do_grab(state: &mut X11State, pos: Position, entry: (i32, i32)) {
    let grab_mask =
        (PointerMotionMask | ButtonPressMask | ButtonReleaseMask | ButtonMotionMask) as u32;
    let result = unsafe {
        XGrabPointer(
            state.display,
            state.root,
            False,
            grab_mask,
            GrabModeAsync,
            GrabModeAsync,
            0, // no confinement
            0, // no cursor change
            CurrentTime,
        )
    };
    if result != GrabSuccess {
        log::warn!("x11: XGrabPointer failed with code {result}");
        return;
    }
    unsafe {
        XGrabKeyboard(
            state.display,
            state.root,
            False,
            GrabModeAsync,
            GrabModeAsync,
            CurrentTime,
        );
        XWarpPointer(state.display, 0, state.root, 0, 0, 0, 0, entry.0, entry.1);
        XFlush(state.display);
    }
    state.entry_point = entry;
    state.active_client = Some(pos);
    let _ = state.event_tx.try_send((pos, CaptureEvent::Begin));
    log::debug!("x11: grabbed pointer for client {pos:?} at {entry:?}");
}

fn do_release(state: &mut X11State) {
    unsafe {
        XUngrabPointer(state.display, CurrentTime);
        XUngrabKeyboard(state.display, CurrentTime);
        XFlush(state.display);
    }
    log::debug!("x11: released pointer (was {:?})", state.active_client);
    state.active_client = None;
}

#[allow(non_upper_case_globals)]
fn handle_event(state: &mut X11State, ev: XEvent) {
    match unsafe { ev.type_ } {
        MotionNotify => {
            let m: XMotionEvent = unsafe { ev.motion };
            handle_motion(state, m);
        }
        ButtonPress => {
            if let Some(pos) = state.active_client {
                let b: XButtonEvent = unsafe { ev.button };
                if let Some(button) = x11_button_to_evdev(b.button) {
                    let _ = state.event_tx.try_send((
                        pos,
                        CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                            time: 0,
                            button,
                            state: 1,
                        })),
                    ));
                }
            }
        }
        ButtonRelease => {
            if let Some(pos) = state.active_client {
                let b: XButtonEvent = unsafe { ev.button };
                if let Some(button) = x11_button_to_evdev(b.button) {
                    let _ = state.event_tx.try_send((
                        pos,
                        CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                            time: 0,
                            button,
                            state: 0,
                        })),
                    ));
                }
            }
        }
        KeyPress => {
            if let Some(pos) = state.active_client {
                let k: XKeyEvent = unsafe { ev.key };
                let _ = state.event_tx.try_send((
                    pos,
                    CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Key {
                        time: 0,
                        key: k.keycode.saturating_sub(8),
                        state: 1,
                    })),
                ));
            }
        }
        KeyRelease => {
            if let Some(pos) = state.active_client {
                let k: XKeyEvent = unsafe { ev.key };
                let _ = state.event_tx.try_send((
                    pos,
                    CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Key {
                        time: 0,
                        key: k.keycode.saturating_sub(8),
                        state: 0,
                    })),
                ));
            }
        }
        _ => {}
    }
}

fn handle_motion(state: &mut X11State, m: XMotionEvent) {
    let curr = (m.x_root, m.y_root);
    let entry = state.entry_point;
    let dx = (curr.0 - entry.0) as f64;
    let dy = (curr.1 - entry.1) as f64;
    // Skip warp-back echo events (XWarpPointer generates a synthetic MotionNotify)
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    unsafe {
        XWarpPointer(state.display, 0, state.root, 0, 0, 0, 0, entry.0, entry.1);
        XFlush(state.display);
    }
    if let Some(pos) = state.active_client {
        let _ = state.event_tx.try_send((
            pos,
            CaptureEvent::Input(Event::Pointer(PointerEvent::Motion {
                time: 0,
                dx,
                dy,
            })),
        ));
    }
}

// ── Pure logic functions ───────────────────────────────────────────────────────

pub(crate) fn crossed_boundary(
    prev: (i32, i32),
    curr: (i32, i32),
    w: i32,
    h: i32,
) -> Option<Position> {
    if prev.0 > 0 && curr.0 <= 0 {
        Some(Position::Left)
    } else if prev.0 < w - 1 && curr.0 >= w - 1 {
        // X11 clamps the cursor to [0, w-1], so >= w is never true.
        // Treat arrival at the rightmost pixel as a right-edge crossing.
        Some(Position::Right)
    } else if prev.1 > 0 && curr.1 <= 0 {
        Some(Position::Top)
    } else if prev.1 < h - 1 && curr.1 >= h - 1 {
        // Same reasoning for the bottom edge.
        Some(Position::Bottom)
    } else {
        None
    }
}

pub(crate) fn clamp_to_screen(pos: (i32, i32), w: i32, h: i32) -> (i32, i32) {
    (pos.0.clamp(0, w - 1), pos.1.clamp(0, h - 1))
}

pub(crate) fn x11_button_to_evdev(button: u32) -> Option<u32> {
    use input_event::{BTN_LEFT, BTN_MIDDLE, BTN_RIGHT};
    match button {
        1 => Some(BTN_LEFT),
        2 => Some(BTN_MIDDLE),
        3 => Some(BTN_RIGHT),
        _ => None,
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crosses_left_boundary() {
        assert_eq!(crossed_boundary((5, 100), (-1, 100), 1920, 1080), Some(Position::Left));
    }

    #[test]
    fn crosses_right_boundary() {
        // X11 clamps to w-1; the cursor arrives at 1919, never at 1920.
        assert_eq!(crossed_boundary((1915, 100), (1919, 100), 1920, 1080), Some(Position::Right));
    }

    #[test]
    fn crosses_top_boundary() {
        assert_eq!(crossed_boundary((100, 5), (100, -1), 1920, 1080), Some(Position::Top));
    }

    #[test]
    fn crosses_bottom_boundary() {
        // X11 clamps to h-1; the cursor arrives at 1079, never at 1080.
        assert_eq!(crossed_boundary((100, 1075), (100, 1079), 1920, 1080), Some(Position::Bottom));
    }

    #[test]
    fn no_crossing_interior_movement() {
        assert_eq!(crossed_boundary((100, 100), (200, 200), 1920, 1080), None);
    }

    #[test]
    fn no_crossing_already_at_left_edge() {
        assert_eq!(crossed_boundary((0, 100), (0, 100), 1920, 1080), None);
    }

    #[test]
    fn no_crossing_already_at_right_edge() {
        // Cursor already at w-1: no prev→curr transition, must not re-trigger.
        assert_eq!(crossed_boundary((1919, 100), (1919, 100), 1920, 1080), None);
    }

    #[test]
    fn clamp_within_bounds_is_identity() {
        assert_eq!(clamp_to_screen((500, 300), 1920, 1080), (500, 300));
    }

    #[test]
    fn clamp_negative_coords() {
        assert_eq!(clamp_to_screen((-10, -5), 1920, 1080), (0, 0));
    }

    #[test]
    fn clamp_over_right_bottom_edge() {
        assert_eq!(clamp_to_screen((2000, 1200), 1920, 1080), (1919, 1079));
    }

    #[test]
    fn left_button_maps_to_btn_left() {
        use input_event::BTN_LEFT;
        assert_eq!(x11_button_to_evdev(1), Some(BTN_LEFT));
    }

    #[test]
    fn right_button_maps_to_btn_right() {
        use input_event::BTN_RIGHT;
        assert_eq!(x11_button_to_evdev(3), Some(BTN_RIGHT));
    }

    #[test]
    fn unknown_button_returns_none() {
        assert_eq!(x11_button_to_evdev(8), None);
    }
}
