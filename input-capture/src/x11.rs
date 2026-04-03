use std::collections::HashSet;
use std::pin::Pin;
use std::ptr;
use std::sync::mpsc as std_mpsc;
use std::task::{Context, Poll};
use std::time::Duration;
use std::thread;

use async_trait::async_trait;
use futures_core::Stream;
use tokio::sync::mpsc;

use input_event::{
    BTN_BACK, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Event, KeyboardEvent, PointerEvent,
};
use x11::xlib;

use super::{Capture, CaptureError, CaptureEvent, Position, error::X11InputCaptureCreationError};

enum Command {
    Create(Position),
    Destroy(Position),
    Release,
    Terminate,
}

pub struct X11InputCapture {
    event_rx: mpsc::Receiver<(Position, CaptureEvent)>,
    cmd_tx: std_mpsc::Sender<Command>,
    wakeup: std::os::unix::net::UnixStream,
    thread: Option<thread::JoinHandle<()>>,
}

unsafe impl Send for X11InputCapture {}

impl X11InputCapture {
    pub fn new() -> Result<Self, X11InputCaptureCreationError> {
        let (event_tx, event_rx) = mpsc::channel(128);
        let (cmd_tx, cmd_rx) = std_mpsc::channel();
        let (wakeup_w, wakeup_r) =
            std::os::unix::net::UnixStream::pair().map_err(X11InputCaptureCreationError::Io)?;
        wakeup_w
            .set_nonblocking(true)
            .map_err(X11InputCaptureCreationError::Io)?;
        wakeup_r
            .set_nonblocking(true)
            .map_err(X11InputCaptureCreationError::Io)?;

        // Open display on current thread to validate X11 availability,
        // then close it. The event loop thread opens its own connection.
        let display = unsafe { xlib::XOpenDisplay(ptr::null()) };
        if display.is_null() {
            return Err(X11InputCaptureCreationError::OpenDisplay);
        }
        unsafe { xlib::XCloseDisplay(display) };

        let thread = thread::spawn(move || {
            event_loop(event_tx, cmd_rx, wakeup_r);
        });

        Ok(Self {
            event_rx,
            cmd_tx,
            wakeup: wakeup_w,
            thread: Some(thread),
        })
    }

    fn send_cmd(&self, cmd: Command) {
        let _ = self.cmd_tx.send(cmd);
        use std::io::Write;
        let _ = (&self.wakeup).write_all(&[1u8]);
    }
}

impl Drop for X11InputCapture {
    fn drop(&mut self) {
        self.send_cmd(Command::Terminate);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[async_trait]
impl Capture for X11InputCapture {
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError> {
        self.send_cmd(Command::Create(pos));
        Ok(())
    }

    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError> {
        self.send_cmd(Command::Destroy(pos));
        Ok(())
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        self.send_cmd(Command::Release);
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        self.send_cmd(Command::Terminate);
        Ok(())
    }
}

impl Stream for X11InputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match std::task::ready!(self.event_rx.poll_recv(cx)) {
            None => Poll::Ready(None),
            Some(e) => Poll::Ready(Some(Ok(e))),
        }
    }
}

// --- X11 Event Loop (runs in dedicated thread) ---

unsafe extern "C" fn x11_error_handler(
    _display: *mut xlib::Display,
    event: *mut xlib::XErrorEvent,
) -> i32 {
    let error_code = (*event).error_code;
    log::warn!("X11 error: code={error_code}");
    0
}

struct X11State {
    display: *mut xlib::Display,
    root: xlib::Window,
    screen_width: i32,
    screen_height: i32,
    blank_cursor: xlib::Cursor,
    clients: HashSet<Position>,
    active_pos: Option<Position>,
    warp_center: (i32, i32),
    entry_point: (i32, i32),
    just_warped: bool,
    event_tx: mpsc::Sender<(Position, CaptureEvent)>,
}

impl X11State {
    fn new(display: *mut xlib::Display, event_tx: mpsc::Sender<(Position, CaptureEvent)>) -> Self {
        unsafe {
            let screen = xlib::XDefaultScreen(display);
            let root = xlib::XDefaultRootWindow(display);
            let screen_width = xlib::XDisplayWidth(display, screen);
            let screen_height = xlib::XDisplayHeight(display, screen);
            let blank_cursor = create_blank_cursor(display, root);

            Self {
                display,
                root,
                screen_width,
                screen_height,
                blank_cursor,
                clients: HashSet::new(),
                active_pos: None,
                warp_center: (screen_width / 2, screen_height / 2),
                entry_point: (0, 0),
                just_warped: false,
                event_tx,
            }
        }
    }

    fn query_pointer(&self) -> (i32, i32) {
        let mut root_return: xlib::Window = 0;
        let mut child_return: xlib::Window = 0;
        let mut root_x: i32 = 0;
        let mut root_y: i32 = 0;
        let mut win_x: i32 = 0;
        let mut win_y: i32 = 0;
        let mut mask_return: u32 = 0;
        unsafe {
            xlib::XQueryPointer(
                self.display,
                self.root,
                &mut root_return,
                &mut child_return,
                &mut root_x,
                &mut root_y,
                &mut win_x,
                &mut win_y,
                &mut mask_return,
            );
        }
        (root_x, root_y)
    }

    fn at_edge(&self, x: i32, y: i32) -> Option<Position> {
        for &pos in &[
            Position::Left,
            Position::Right,
            Position::Top,
            Position::Bottom,
        ] {
            if !self.clients.contains(&pos) {
                continue;
            }
            let at_edge = match pos {
                Position::Left => x <= 0,
                Position::Right => x >= self.screen_width - 1,
                Position::Top => y <= 0,
                Position::Bottom => y >= self.screen_height - 1,
            };
            if at_edge {
                return Some(pos);
            }
        }
        None
    }

    fn start_capture(&mut self, pos: Position, x: i32, y: i32) {
        log::info!("X11: starting capture at {pos} ({x}, {y})");
        self.active_pos = Some(pos);
        self.entry_point = (x, y);

        unsafe {
            let mask = (xlib::PointerMotionMask
                | xlib::ButtonPressMask
                | xlib::ButtonReleaseMask) as u32;

            let grab_result = xlib::XGrabPointer(
                self.display,
                self.root,
                xlib::False,
                mask,
                xlib::GrabModeAsync,
                xlib::GrabModeAsync,
                0, // no confine
                self.blank_cursor,
                xlib::CurrentTime,
            );

            if grab_result != xlib::GrabSuccess {
                log::warn!("X11: XGrabPointer failed: {grab_result}");
                self.active_pos = None;
                return;
            }

            let kb_result = xlib::XGrabKeyboard(
                self.display,
                self.root,
                xlib::False,
                xlib::GrabModeAsync,
                xlib::GrabModeAsync,
                xlib::CurrentTime,
            );

            if kb_result != xlib::GrabSuccess {
                log::warn!("X11: XGrabKeyboard failed: {kb_result}");
                xlib::XUngrabPointer(self.display, xlib::CurrentTime);
                self.active_pos = None;
                return;
            }

            // Warp to center of screen
            xlib::XWarpPointer(
                self.display,
                0,
                self.root,
                0,
                0,
                0,
                0,
                self.warp_center.0,
                self.warp_center.1,
            );
            xlib::XFlush(self.display);
            self.just_warped = true;
        }

        let _ = self.event_tx.blocking_send((pos, CaptureEvent::Begin));
    }

    fn stop_capture(&mut self) {
        if self.active_pos.is_none() {
            return;
        }
        let pos = self.active_pos.take().unwrap();
        log::info!("X11: stopping capture");
        self.just_warped = false;

        // Move cursor a few pixels inward from the edge to avoid immediate re-capture
        let margin = 20;
        let (mut x, mut y) = self.entry_point;
        match pos {
            Position::Left => x = x.saturating_add(margin),
            Position::Right => x = (x - margin).max(0),
            Position::Top => y = y.saturating_add(margin),
            Position::Bottom => y = (y - margin).max(0),
        }

        unsafe {
            xlib::XUngrabPointer(self.display, xlib::CurrentTime);
            xlib::XUngrabKeyboard(self.display, xlib::CurrentTime);

            xlib::XWarpPointer(
                self.display,
                0,
                self.root,
                0,
                0,
                0,
                0,
                x,
                y,
            );
            xlib::XFlush(self.display);
        }
    }

    fn handle_motion(&mut self, x_root: i32, y_root: i32) {
        let Some(pos) = self.active_pos else {
            return;
        };

        if self.just_warped {
            if x_root == self.warp_center.0 && y_root == self.warp_center.1 {
                self.just_warped = false;
                return;
            }
            // Warp event had different coords than expected - process it anyway
            self.just_warped = false;
        }

        let dx = (x_root - self.warp_center.0) as f64;
        let dy = (y_root - self.warp_center.1) as f64;

        if dx != 0.0 || dy != 0.0 {
            let event = CaptureEvent::Input(Event::Pointer(PointerEvent::Motion {
                time: 0,
                dx,
                dy,
            }));
            let _ = self.event_tx.blocking_send((pos, event));
        }

        // Warp back to center
        unsafe {
            xlib::XWarpPointer(
                self.display,
                0,
                self.root,
                0,
                0,
                0,
                0,
                self.warp_center.0,
                self.warp_center.1,
            );
            xlib::XFlush(self.display);
            self.just_warped = true;
        }
    }

    fn handle_button(&mut self, button: u32, pressed: bool) {
        let Some(pos) = self.active_pos else {
            return;
        };

        let state = u32::from(pressed);

        let event = match button {
            1 => Some(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_LEFT,
                state,
            }))),
            2 => Some(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_MIDDLE,
                state,
            }))),
            3 => Some(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_RIGHT,
                state,
            }))),
            // Scroll events only trigger on press in X11
            4 if pressed => Some(CaptureEvent::Input(Event::Pointer(
                PointerEvent::AxisDiscrete120 {
                    axis: 0,
                    value: -120,
                },
            ))),
            5 if pressed => Some(CaptureEvent::Input(Event::Pointer(
                PointerEvent::AxisDiscrete120 {
                    axis: 0,
                    value: 120,
                },
            ))),
            6 if pressed => Some(CaptureEvent::Input(Event::Pointer(
                PointerEvent::AxisDiscrete120 {
                    axis: 1,
                    value: -120,
                },
            ))),
            7 if pressed => Some(CaptureEvent::Input(Event::Pointer(
                PointerEvent::AxisDiscrete120 {
                    axis: 1,
                    value: 120,
                },
            ))),
            8 => Some(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_BACK,
                state,
            }))),
            9 => Some(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_FORWARD,
                state,
            }))),
            _ => None,
        };

        if let Some(event) = event {
            let _ = self.event_tx.blocking_send((pos, event));
        }
    }

    fn handle_key(&mut self, keycode: u32, pressed: bool) {
        let Some(pos) = self.active_pos else {
            return;
        };

        // X11 keycodes are evdev keycodes + 8
        let linux_key = keycode.saturating_sub(8);
        let state = u8::from(pressed);

        let event = CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Key {
            time: 0,
            key: linux_key,
            state,
        }));
        let _ = self.event_tx.blocking_send((pos, event));
    }

    /// Returns true if the event loop should exit.
    fn handle_command(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::Create(pos) => {
                log::debug!("X11: creating capture for {pos}");
                self.clients.insert(pos);
            }
            Command::Destroy(pos) => {
                log::debug!("X11: destroying capture for {pos}");
                if self.active_pos == Some(pos) {
                    self.stop_capture();
                }
                self.clients.remove(&pos);
            }
            Command::Release => {
                self.stop_capture();
            }
            Command::Terminate => {
                self.stop_capture();
                return true;
            }
        }
        false
    }
}

impl Drop for X11State {
    fn drop(&mut self) {
        unsafe {
            xlib::XFreeCursor(self.display, self.blank_cursor);
            xlib::XCloseDisplay(self.display);
        }
    }
}

unsafe fn create_blank_cursor(
    display: *mut xlib::Display,
    root: xlib::Window,
) -> xlib::Cursor {
    let data: [u8; 1] = [0];
    let bitmap =
        xlib::XCreateBitmapFromData(display, root, data.as_ptr() as *const std::ffi::c_char, 1, 1);
    let mut color: xlib::XColor = std::mem::zeroed();
    let cursor =
        xlib::XCreatePixmapCursor(display, bitmap, bitmap, &mut color, &mut color, 0, 0);
    xlib::XFreePixmap(display, bitmap);
    cursor
}

fn event_loop(
    event_tx: mpsc::Sender<(Position, CaptureEvent)>,
    cmd_rx: std_mpsc::Receiver<Command>,
    mut wakeup: std::os::unix::net::UnixStream,
) {
    unsafe {
        xlib::XSetErrorHandler(Some(x11_error_handler));
    }

    let display = unsafe { xlib::XOpenDisplay(ptr::null()) };
    if display.is_null() {
        log::error!("X11: failed to open display in event loop thread");
        return;
    }

    let mut state = X11State::new(display, event_tx);
    let mut event: xlib::XEvent = unsafe { std::mem::zeroed() };

    loop {
        let mut did_work = false;

        // Drain wakeup pipe
        {
            use std::io::Read;
            let mut buf = [0u8; 64];
            let _ = wakeup.read(&mut buf);
        }

        // Process commands
        while let Ok(cmd) = cmd_rx.try_recv() {
            did_work = true;
            if state.handle_command(cmd) {
                return;
            }
        }

        // Process all pending X11 events
        while unsafe { xlib::XPending(display) } > 0 {
            did_work = true;
            unsafe {
                xlib::XNextEvent(display, &mut event);
            }

            match event.get_type() {
                xlib::MotionNotify => {
                    let motion = unsafe { event.motion };
                    state.handle_motion(motion.x_root, motion.y_root);
                }
                xlib::ButtonPress => {
                    let button = unsafe { event.button };
                    state.handle_button(button.button, true);
                }
                xlib::ButtonRelease => {
                    let button = unsafe { event.button };
                    state.handle_button(button.button, false);
                }
                xlib::KeyPress => {
                    let key = unsafe { event.key };
                    state.handle_key(key.keycode, true);
                }
                xlib::KeyRelease => {
                    let key = unsafe { event.key };
                    state.handle_key(key.keycode, false);
                }
                _ => {}
            }
        }

        // Detection mode: poll cursor position when not captured
        if state.active_pos.is_none() {
            let (x, y) = state.query_pointer();
            if let Some(pos) = state.at_edge(x, y) {
                state.start_capture(pos, x, y);
                did_work = true;
            }
        }

        if !did_work {
            thread::sleep(Duration::from_millis(2));
        }
    }
}
