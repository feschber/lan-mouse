use super::{Capture, CaptureError, CaptureEvent, Position, error::MacosCaptureCreationError};
use async_trait::async_trait;
use bitflags::bitflags;
use core_foundation::{
    base::{CFRelease, TCFType, kCFAllocatorDefault},
    date::CFTimeInterval,
    number::{CFBooleanRef, kCFBooleanTrue},
    runloop::{CFRunLoop, CFRunLoopSource, kCFRunLoopCommonModes},
    string::{CFStringCreateWithCString, CFStringRef, kCFStringEncodingUTF8},
};
use core_graphics::{
    base::{CGError, kCGErrorSuccess},
    display::{CGDisplay, CGPoint},
    event::{
        CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventTapProxy, CGEventType, CallbackResult, EventField,
    },
    event_source::{CGEventSource, CGEventSourceStateID},
};
use futures_core::Stream;
use input_event::{
    BTN_BACK, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Event, KeyboardEvent, PointerEvent,
};
use keycode::{KeyMap, KeyMapping};
use libc::c_void;
use once_cell::unsync::Lazy;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    collections::{HashSet, VecDeque},
    ffi::{CString, c_char},
    pin::Pin,
    sync::{Arc, Mutex as QueueMutex, OnceLock},
    task::{Context, Poll},
    thread::{self},
    time::{Duration, Instant},
};
use tokio::sync::{
    Mutex,
    mpsc::{self, Sender},
    oneshot,
};

// Events synthesized by Lan Mouse are posted at the HID event tap and are
// visible to our session event tap again. Marked motion may signal that an
// incoming pointer wants to return, but must not start a new outgoing capture.
const LAN_MOUSE_EVENT_MARKER: i64 = 0x4c41_4e4d_4f55_5345;

type QueuedEvent = (Position, CaptureEvent, bool);

#[derive(Default)]
struct CaptureQueue {
    events: QueueMutex<(VecDeque<QueuedEvent>, bool)>,
    waker: futures::task::AtomicWaker,
    failed: AtomicBool,
}

impl CaptureQueue {
    fn push(&self, event: QueuedEvent) -> Result<(), &'static str> {
        let deadline = Instant::now() + Duration::from_millis(2);
        let mut queue = loop {
            if let Ok(queue) = self.events.try_lock() {
                break queue;
            }
            if Instant::now() >= deadline {
                return Err("capture queue lock deadline exceeded");
            }
            thread::sleep(Duration::from_micros(50));
        };
        if queue.1 {
            return Err("capture queue closed");
        }
        if let Some((
            pos,
            CaptureEvent::Input(Event::Pointer(PointerEvent::Motion { time, dx, dy })),
            route,
        )) = queue.0.back_mut()
        {
            if let (
                new_pos,
                CaptureEvent::Input(Event::Pointer(PointerEvent::Motion {
                    time: new_time,
                    dx: new_dx,
                    dy: new_dy,
                })),
                new_route,
            ) = event
            {
                if *pos == new_pos && *route == new_route {
                    *time = new_time;
                    *dx += new_dx;
                    *dy += new_dy;
                    drop(queue);
                    self.waker.wake();
                    return Ok(());
                }
            }
        }
        if queue.0.len() >= 32 {
            return Err("capture queue capacity exceeded");
        }
        queue.0.push_back(event);
        drop(queue);
        self.waker.wake();
        Ok(())
    }

    fn poll(&self, cx: &mut Context<'_>) -> Poll<Option<QueuedEvent>> {
        self.waker.register(cx.waker());
        // A failure must reach the consumer even when either bounded queue is
        // full or its mutex is unavailable. Never deliver stale input first.
        if self.failed.load(Ordering::Acquire) {
            return Poll::Ready(None);
        }
        let Ok(mut queue) = self.events.try_lock() else {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        };
        if let Some(event) = queue.0.pop_front() {
            Poll::Ready(Some(event))
        } else if queue.1 {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }

    fn close(&self) {
        self.events.lock().unwrap().1 = true;
        self.waker.wake();
    }

    fn fail(&self) {
        self.failed.store(true, Ordering::Release);
        self.waker.wake();
    }

    fn poll_result(&self, cx: &mut Context<'_>) -> Poll<Option<Result<QueuedEvent, CaptureError>>> {
        match self.poll(cx) {
            Poll::Ready(None) if self.failed.load(Ordering::Acquire) => {
                Poll::Ready(Some(Err(CaptureError::EventTapDisabled)))
            }
            Poll::Ready(event) => Poll::Ready(event.map(Ok)),
            Poll::Pending => Poll::Pending,
        }
    }
}

struct CallbackTiming<'a> {
    started: Instant,
    port: &'a OnceLock<usize>,
    notify: &'a Sender<ProducerEvent>,
    events: &'a CaptureQueue,
    cursor: &'a CursorVisibility,
}

impl Drop for CallbackTiming<'_> {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed();
        if elapsed >= Duration::from_secs(1) {
            log::warn!(
                "macOS input callback exceeded budget: elapsed_us={}",
                elapsed.as_micros()
            );
            disable_capture_tap(
                self.port,
                self.notify,
                self.events,
                self.cursor,
                "input callback exceeded one second",
            );
        }
    }
}

// A Quartz callback must not wait indefinitely for the async consumer.
fn callback_lock<T>(state: &Mutex<T>) -> Option<tokio::sync::MutexGuard<'_, T>> {
    let deadline = Instant::now() + Duration::from_millis(2);
    loop {
        if let Ok(guard) = state.try_lock() {
            return Some(guard);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_micros(50));
    }
}

fn record_capture_failure(
    notify: &Sender<ProducerEvent>,
    events: &CaptureQueue,
    cursor: &CursorVisibility,
) {
    events.fail();
    // Do not acquire the capture-state lock to restore the local cursor.
    let _ = cursor.set_hidden(false);
    let _ = notify.try_send(ProducerEvent::EventTapDisabled);
}

fn disable_capture_tap(
    port: &OnceLock<usize>,
    notify: &Sender<ProducerEvent>,
    events: &CaptureQueue,
    cursor: &CursorVisibility,
    reason: &str,
) {
    log::error!("disabling capture to restore local input: {reason}");
    record_capture_failure(notify, events, cursor);
    if let Some(&port) = port.get() {
        unsafe { CGEventTapEnable(port as *mut c_void, false) };
    }
}

#[derive(Debug, Default)]
struct Bounds {
    xmin: f64,
    xmax: f64,
    ymin: f64,
    ymax: f64,
}

#[derive(Clone, Debug)]
struct CursorVisibility {
    hidden: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    wake: std::sync::mpsc::SyncSender<()>,
}

impl CursorVisibility {
    fn new(
        mut apply: impl FnMut(bool) -> Result<(), CaptureError> + Send + 'static,
        on_error: impl FnOnce(CaptureError) + Send + 'static,
    ) -> Self {
        let hidden = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let desired = hidden.clone();
        let failed_worker = failed.clone();
        let (wake, wake_rx) = std::sync::mpsc::sync_channel(1);
        thread::spawn(move || {
            let mut applied_hidden = false;
            while wake_rx.recv().is_ok() {
                while wake_rx.try_recv().is_ok() {}
                let next = desired.load(Ordering::Acquire);
                if next != applied_hidden {
                    let started = Instant::now();
                    let result = apply(next);
                    if started.elapsed() >= Duration::from_millis(2) {
                        log::warn!(
                            "macOS cursor visibility call: hidden={next}, elapsed_us={}",
                            started.elapsed().as_micros()
                        );
                    }
                    if let Err(error) = result {
                        failed_worker.store(true, Ordering::Release);
                        // A failed hide may have partially taken effect.
                        let _ = apply(false);
                        on_error(error);
                        return;
                    }
                    applied_hidden = next;
                }
            }
            if applied_hidden {
                if let Err(error) = apply(false) {
                    failed_worker.store(true, Ordering::Release);
                    on_error(error);
                }
            }
        });
        Self {
            hidden,
            failed,
            wake,
        }
    }

    fn set_hidden(&self, hidden: bool) -> Result<(), CaptureError> {
        if self.failed.load(Ordering::Acquire) {
            return Err(CaptureError::EventTapDisabled);
        }
        // Publish while holding the state lock, keeping visibility requests in
        // state-transition order. The worker never takes the state lock.
        self.hidden.store(hidden, Ordering::Release);
        match self.wake.try_send(()) {
            Ok(()) | Err(std::sync::mpsc::TrySendError::Full(())) => Ok(()),
            Err(std::sync::mpsc::TrySendError::Disconnected(())) => {
                Err(CaptureError::EventTapDisabled)
            }
        }
    }
}

#[derive(Debug)]
struct InputCaptureState {
    cursor: CursorVisibility,
    /// active capture positions
    active_clients: Lazy<HashSet<Position>>,
    /// positions used to return an incoming emulated pointer
    enter_only_clients: HashSet<Position>,
    /// positions that physical local input may currently enter
    ///
    /// A position is disarmed after crossing and is only re-armed once a
    /// physical pointer produces a separate inward motion.
    /// This prevents macOS cursor warps at the edge from immediately starting
    /// the opposite capture after an incoming pointer returns.
    physical_armed_clients: HashSet<Position>,
    /// positions that emulated input may currently use as a return signal
    ///
    /// Incoming emulation starts with a cursor warp at the entry edge. That
    /// warp must not immediately signal a return. Emulated input is armed only
    /// after it moves into the desktop and is disarmed after one return signal.
    emulated_armed_clients: HashSet<Position>,
    /// the currently entered capture position, if any
    current_pos: Option<Position>,
    /// provenance of events belonging to the active capture
    current_source: Option<InputSource>,
    /// position where the cursor was captured
    enter_position: Option<CGPoint>,
    /// bounds of the input capture area
    bounds: Bounds,
    /// current state of modifier keys
    modifier_state: XMods,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputSource {
    Physical,
    Emulated,
}

#[derive(Debug)]
enum ProducerEvent {
    Release,
    Create(Position),
    Destroy(Position),
    SetEnterOnly(Position, bool),
    Grab(Position, InputSource),
    EventTapDisabled,
    DisplayReconfigured,
}

impl InputCaptureState {
    fn new(cursor: CursorVisibility) -> Result<Self, MacosCaptureCreationError> {
        Ok(Self::with_bounds(cursor, Self::display_bounds()?))
    }

    fn with_bounds(cursor: CursorVisibility, bounds: Bounds) -> Self {
        Self {
            cursor,
            active_clients: Lazy::new(HashSet::new),
            enter_only_clients: HashSet::new(),
            physical_armed_clients: HashSet::new(),
            emulated_armed_clients: HashSet::new(),
            current_pos: None,
            current_source: None,
            enter_position: None,
            bounds,
            modifier_state: Default::default(),
        }
    }

    fn arm_clients_on_inward_motion(&mut self, event: &CGEvent, emulated: bool) {
        let relative_x = event.get_double_value_field(EventField::MOUSE_EVENT_DELTA_X);
        let relative_y = event.get_double_value_field(EventField::MOUSE_EVENT_DELTA_Y);

        for &position in self.active_clients.iter() {
            if is_inward_motion(position, relative_x, relative_y) {
                if emulated {
                    self.emulated_armed_clients.insert(position);
                } else {
                    if self.physical_armed_clients.insert(position) {
                        log::debug!("physical capture rearmed at {position}");
                    }
                }
            }
        }
    }

    fn crossing_position(&self, event: &CGEvent) -> Option<Position> {
        let location = event.location();
        let relative_x = event.get_double_value_field(EventField::MOUSE_EVENT_DELTA_X);
        let relative_y = event.get_double_value_field(EventField::MOUSE_EVENT_DELTA_Y);

        for &position in self.active_clients.iter() {
            if (position == Position::Left && (location.x + relative_x) <= self.bounds.xmin)
                || (position == Position::Right && (location.x + relative_x) >= self.bounds.xmax)
                || (position == Position::Top && (location.y + relative_y) <= self.bounds.ymin)
                || (position == Position::Bottom && (location.y + relative_y) >= self.bounds.ymax)
            {
                log::debug!("Crossed barrier into position: {position:?}");
                return Some(position);
            }
        }
        None
    }

    // Get the max bounds of all displays
    fn display_bounds() -> Result<Bounds, MacosCaptureCreationError> {
        let mut capture_bounds = Bounds::default();
        let active_ids =
            CGDisplay::active_displays().map_err(MacosCaptureCreationError::ActiveDisplays)?;
        active_ids.iter().for_each(|d| {
            let bounds = CGDisplay::new(*d).bounds();
            capture_bounds.xmin = capture_bounds.xmin.min(bounds.origin.x);
            capture_bounds.xmax = capture_bounds.xmax.max(bounds.origin.x + bounds.size.width);
            capture_bounds.ymin = capture_bounds.ymin.min(bounds.origin.y);
            capture_bounds.ymax = capture_bounds
                .ymax
                .max(bounds.origin.y + bounds.size.height);
        });

        log::debug!("Updated displays bounds: {capture_bounds:?}");
        Ok(capture_bounds)
    }

    /// start the input capture by
    fn start_capture(&mut self, event: &CGEvent, position: Position) -> CGPoint {
        let mut location = event.location();
        let edge_offset = 1.0;
        // move cursor location to display bounds
        match position {
            Position::Left => location.x = self.bounds.xmin + edge_offset,
            Position::Right => location.x = self.bounds.xmax - edge_offset,
            Position::Top => location.y = self.bounds.ymin + edge_offset,
            Position::Bottom => location.y = self.bounds.ymax - edge_offset,
        };
        self.enter_position = Some(location);
        location
    }

    fn handle_producer_event(&mut self, producer_event: ProducerEvent) -> Result<(), CaptureError> {
        log::debug!("handling event: {producer_event:?}");
        match producer_event {
            ProducerEvent::Release => {
                log::debug!(
                    "release state: position={:?}, source={:?}, physical_armed={:?}",
                    self.current_pos,
                    self.current_source,
                    self.physical_armed_clients
                );
                if self.current_pos.is_some() {
                    self.current_pos = None;
                    self.cursor.set_hidden(false)?;
                }
                self.current_source = None;
            }
            ProducerEvent::Grab(pos, source) => {
                if self.current_pos.is_none() {
                    self.cursor.set_hidden(true)?;
                    self.current_pos = Some(pos);
                    self.current_source = Some(source);
                }
            }
            ProducerEvent::Create(p) => {
                self.active_clients.insert(p);
                self.physical_armed_clients.insert(p);
            }
            ProducerEvent::Destroy(p) => {
                self.physical_armed_clients.remove(&p);
                self.emulated_armed_clients.remove(&p);
                self.enter_only_clients.remove(&p);
                if let Some(current) = self.current_pos {
                    if current == p {
                        self.current_pos = None;
                        self.current_source = None;
                        self.cursor.set_hidden(false)?;
                    };
                }
                self.active_clients.remove(&p);
            }
            ProducerEvent::SetEnterOnly(p, enabled) => {
                if enabled {
                    self.enter_only_clients.insert(p);
                } else {
                    self.enter_only_clients.remove(&p);
                }
            }
            ProducerEvent::EventTapDisabled => {
                // Tap death can happen mid-capture (TCC Accessibility
                // revoked, tap-timeout, etc). Release state so we
                // don't leave the cursor hidden even if the outer
                // task only logs this error rather than propagating.
                if self.current_pos.is_some() {
                    self.current_pos = None;
                    self.cursor.set_hidden(false)?;
                }
                self.current_source = None;
                return Err(CaptureError::EventTapDisabled);
            }
            ProducerEvent::DisplayReconfigured => {
                // The macOS display configuration changed — a monitor
                // was plugged in/out, the resolution changed, the
                // arrangement was rearranged, etc. Re-fetch the
                // active-display bounds so barrier crossings and the
                // cursor-warp on capture-start use the current
                // geometry instead of whatever was true at process
                // start.
                unreachable!("display queries must run outside the capture state lock");
            }
        };
        Ok(())
    }
}

async fn apply_producer_event(
    state: &Mutex<InputCaptureState>,
    event: ProducerEvent,
) -> Result<(), CaptureError> {
    if matches!(event, ProducerEvent::DisplayReconfigured) {
        match InputCaptureState::display_bounds() {
            Ok(bounds) => {
                log::info!("display reconfigured: {bounds:?}");
                state.lock().await.bounds = bounds;
            }
            Err(error) => log::warn!("failed to refresh display bounds: {error}"),
        }
        return Ok(());
    }
    state.lock().await.handle_producer_event(event)
}

fn is_inward_motion(position: Position, relative_x: f64, relative_y: f64) -> bool {
    match position {
        Position::Left => relative_x > 0.0,
        Position::Right => relative_x < 0.0,
        Position::Top => relative_y > 0.0,
        Position::Bottom => relative_y < 0.0,
    }
}

fn is_pointer_motion_event(event_type: CGEventType) -> bool {
    matches!(
        event_type,
        CGEventType::MouseMoved
            | CGEventType::LeftMouseDragged
            | CGEventType::RightMouseDragged
            | CGEventType::OtherMouseDragged
    )
}

fn get_events(
    ev_type: &CGEventType,
    ev: &CGEvent,
    result: &mut Vec<CaptureEvent>,
    modifier_state: &mut XMods,
) -> Result<(), CaptureError> {
    fn map_pointer_event(ev: &CGEvent) -> PointerEvent {
        PointerEvent::Motion {
            time: 0,
            dx: ev.get_double_value_field(EventField::MOUSE_EVENT_DELTA_X),
            dy: ev.get_double_value_field(EventField::MOUSE_EVENT_DELTA_Y),
        }
    }

    fn map_key(ev: &CGEvent) -> Result<u32, CaptureError> {
        let code = ev.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
        match KeyMap::from_key_mapping(KeyMapping::Mac(code as u16)) {
            Ok(k) => Ok(k.evdev as u32),
            Err(()) => Err(CaptureError::KeyMapError(code)),
        }
    }

    match ev_type {
        CGEventType::KeyDown => {
            let k = map_key(ev)?;
            result.push(CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Key {
                time: 0,
                key: k,
                state: 1,
            })));
        }
        CGEventType::KeyUp => {
            let k = map_key(ev)?;
            result.push(CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Key {
                time: 0,
                key: k,
                state: 0,
            })));
        }
        CGEventType::FlagsChanged => {
            let mut depressed = XMods::empty();
            let mut mods_locked = XMods::empty();
            let cg_flags = ev.get_flags();

            if cg_flags.contains(CGEventFlags::CGEventFlagShift) {
                depressed |= XMods::ShiftMask;
            }
            if cg_flags.contains(CGEventFlags::CGEventFlagControl) {
                depressed |= XMods::ControlMask;
            }
            if cg_flags.contains(CGEventFlags::CGEventFlagAlternate) {
                depressed |= XMods::Mod1Mask;
            }
            if cg_flags.contains(CGEventFlags::CGEventFlagCommand) {
                depressed |= XMods::Mod4Mask;
            }
            if cg_flags.contains(CGEventFlags::CGEventFlagAlphaShift) {
                depressed |= XMods::LockMask;
                mods_locked |= XMods::LockMask;
            }

            // check if pressed or released
            let state = if depressed > *modifier_state { 1 } else { 0 };
            *modifier_state = depressed;

            if let Ok(key) = map_key(ev) {
                let key_event = CaptureEvent::Input(Event::Keyboard(KeyboardEvent::Key {
                    time: 0,
                    key,
                    state,
                }));
                result.push(key_event);
            }

            let modifier_event = KeyboardEvent::Modifiers {
                depressed: depressed.bits(),
                latched: 0,
                locked: mods_locked.bits(),
                group: 0,
            };

            result.push(CaptureEvent::Input(Event::Keyboard(modifier_event)));
        }
        CGEventType::MouseMoved => {
            result.push(CaptureEvent::Input(Event::Pointer(map_pointer_event(ev))))
        }
        CGEventType::LeftMouseDragged => {
            result.push(CaptureEvent::Input(Event::Pointer(map_pointer_event(ev))))
        }
        CGEventType::RightMouseDragged => {
            result.push(CaptureEvent::Input(Event::Pointer(map_pointer_event(ev))))
        }
        CGEventType::OtherMouseDragged => {
            result.push(CaptureEvent::Input(Event::Pointer(map_pointer_event(ev))))
        }
        CGEventType::LeftMouseDown => {
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_LEFT,
                state: 1,
            })))
        }
        CGEventType::LeftMouseUp => {
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_LEFT,
                state: 0,
            })))
        }
        CGEventType::RightMouseDown => {
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_RIGHT,
                state: 1,
            })))
        }
        CGEventType::RightMouseUp => {
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_RIGHT,
                state: 0,
            })))
        }
        CGEventType::OtherMouseDown => {
            let btn_num = ev.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            let button = match btn_num {
                3 => BTN_BACK,
                4 => BTN_FORWARD,
                _ => BTN_MIDDLE,
            };
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button,
                state: 1,
            })))
        }
        CGEventType::OtherMouseUp => {
            let btn_num = ev.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER);
            let button = match btn_num {
                3 => BTN_BACK,
                4 => BTN_FORWARD,
                _ => BTN_MIDDLE,
            };
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button,
                state: 0,
            })))
        }
        CGEventType::ScrollWheel => {
            if ev.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_IS_CONTINUOUS) != 0 {
                let v =
                    ev.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1);
                let h =
                    ev.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2);
                if v != 0 {
                    result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Axis {
                        time: 0,
                        axis: 0, // Vertical
                        value: v as f64,
                    })));
                }
                if h != 0 {
                    result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Axis {
                        time: 0,
                        axis: 1, // Horizontal
                        value: h as f64,
                    })));
                }
            } else {
                // line based scrolling
                const LINES_PER_STEP: i32 = 3;
                const V120_STEPS_PER_LINE: i32 = 120 / LINES_PER_STEP;
                let v = ev.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1);
                let h = ev.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2);
                if v != 0 {
                    result.push(CaptureEvent::Input(Event::Pointer(
                        PointerEvent::AxisDiscrete120 {
                            axis: 0, // Vertical
                            value: V120_STEPS_PER_LINE * v as i32,
                        },
                    )));
                }
                if h != 0 {
                    result.push(CaptureEvent::Input(Event::Pointer(
                        PointerEvent::AxisDiscrete120 {
                            axis: 1, // Horizontal
                            value: V120_STEPS_PER_LINE * h as i32,
                        },
                    )));
                }
            }
        }
        _ => (),
    }
    Ok(())
}

fn create_event_tap<'a>(
    client_state: Arc<Mutex<InputCaptureState>>,
    cursor: CursorVisibility,
    notify_tx: Sender<ProducerEvent>,
    event_tx: Arc<CaptureQueue>,
) -> Result<CGEventTap<'a>, MacosCaptureCreationError> {
    // Shared slot for the tap's mach port pointer. Stored as `usize`
    // because raw pointers aren't `Send`, but the integer
    // representation is — and CGEventTapEnable is documented as
    // thread-safe. Set immediately after CGEventTap::new returns;
    // read by the callback to disable capture when processing cannot keep up.
    let tap_mach_port: Arc<OnceLock<usize>> = Arc::new(OnceLock::new());
    let tap_mach_port_cb = Arc::clone(&tap_mach_port);

    let cg_events_of_interest: Vec<CGEventType> = vec![
        CGEventType::LeftMouseDown,
        CGEventType::LeftMouseUp,
        CGEventType::RightMouseDown,
        CGEventType::RightMouseUp,
        CGEventType::OtherMouseDown,
        CGEventType::OtherMouseUp,
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::OtherMouseDragged,
        CGEventType::ScrollWheel,
        CGEventType::KeyDown,
        CGEventType::KeyUp,
        CGEventType::FlagsChanged,
    ];

    let event_tap_callback = move |_proxy: CGEventTapProxy,
                                   event_type: CGEventType,
                                   cg_ev: &CGEvent| {
        let _timing = CallbackTiming {
            started: Instant::now(),
            port: &tap_mach_port_cb,
            notify: &notify_tx,
            events: &event_tx,
            cursor: &cursor,
        };
        log::trace!("Got event from tap: {event_type:?}");

        if matches!(
            event_type,
            CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
        ) {
            record_capture_failure(&notify_tx, &event_tx, &cursor);
            return CallbackResult::Keep;
        }
        if event_tx.failed.load(Ordering::Acquire) {
            return CallbackResult::Keep;
        }
        let Some(mut state) = callback_lock(&client_state) else {
            disable_capture_tap(
                &tap_mach_port_cb,
                &notify_tx,
                &event_tx,
                &cursor,
                "capture-state lock deadline exceeded",
            );
            return CallbackResult::Keep;
        };
        if state.cursor.failed.load(Ordering::Acquire) {
            drop(state);
            disable_capture_tap(
                &tap_mach_port_cb,
                &notify_tx,
                &event_tx,
                &cursor,
                "cursor visibility worker failed",
            );
            return CallbackResult::Keep;
        }
        let mut warp_position = None;
        let mut capture_position = None;
        let mut res_events = vec![];
        let mut drop_event = false;
        let mut route_enter_only = false;
        let source_user_data = cg_ev.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA);
        let source_pid = cg_ev.get_integer_value_field(EventField::EVENT_SOURCE_UNIX_PROCESS_ID);
        let own_pid = i64::from(std::process::id());
        let is_lan_mouse_event =
            source_user_data == LAN_MOUSE_EVENT_MARKER || source_pid == own_pid;
        let input_source = if is_lan_mouse_event {
            InputSource::Emulated
        } else {
            InputSource::Physical
        };

        // Are we in a client?
        if let Some(current_pos) = state.current_pos {
            // Do not fold events from the other source into an active capture.
            // Physical and remotely emulated pointers may both be producing
            // events in the same session.
            if state.current_source != Some(input_source) {
                return CallbackResult::Keep;
            }

            capture_position = Some(current_pos);
            drop_event = true;
            get_events(
                &event_type,
                cg_ev,
                &mut res_events,
                &mut state.modifier_state,
            )
            .unwrap_or_else(|e| {
                log::error!("Failed to get events: {e}");
            });

            // Keep (hidden) cursor at the edge of the screen
            if is_pointer_motion_event(event_type) {
                warp_position = state.enter_position;
            }
        } else if is_pointer_motion_event(event_type) {
            // Did we cross a barrier?
            if let Some(new_pos) = state.crossing_position(cg_ev) {
                let crossing_is_armed = if input_source == InputSource::Emulated {
                    state.emulated_armed_clients.remove(&new_pos)
                } else {
                    state.physical_armed_clients.remove(&new_pos)
                };
                if crossing_is_armed {
                    log::debug!(
                        "pointer crossed {new_pos}: source_pid={source_pid}, own_pid={own_pid}, \
                         source_user_data={source_user_data}, synthesized={is_lan_mouse_event}"
                    );
                    capture_position = Some(new_pos);
                    if input_source == InputSource::Emulated
                        && state.enter_only_clients.contains(&new_pos)
                    {
                        // Let only the EnterOnly handle notify the remote sender
                        // that its pointer crossed back. Do not grab/hide the
                        // local cursor and do not drop the synthesized event.
                        route_enter_only = true;
                        res_events.push(CaptureEvent::Begin);
                    } else {
                        drop_event = true;
                        warp_position = Some(state.start_capture(cg_ev, new_pos));
                        res_events.push(CaptureEvent::Begin);
                        state
                            .handle_producer_event(ProducerEvent::Grab(new_pos, input_source))
                            .unwrap_or_else(|e| log::warn!("failed to grab pointer: {e}"));
                    }
                } else {
                    log::debug!(
                        "ignoring disarmed {} crossing at {new_pos}",
                        if is_lan_mouse_event {
                            "emulated"
                        } else {
                            "physical"
                        }
                    );
                }
            } else {
                // Rearm on the first separate inward motion. The initial
                // outward crossing stays disarmed while even a short
                // inward-then-return gesture works.
                state.arm_clients_on_inward_motion(cg_ev, input_source == InputSource::Emulated);
            }
        }

        // Release the mutex before enqueueing events. If the bounded queue is
        // full, stop capture rather than continuing with missing key/button events.
        drop(state);

        // Quartz can wait for WindowServer. Never hold the shared state lock
        // across a synchronous cursor operation.
        if let Some(position) = warp_position {
            if let Err(error) = CGDisplay::warp_mouse_cursor_position(position) {
                log::warn!("failed to warp mouse cursor: {error}");
            }
        }

        if let Some(pos) = capture_position {
            for e in &res_events {
                if let Err(reason) = event_tx.push((pos, *e, route_enter_only)) {
                    disable_capture_tap(&tap_mach_port_cb, &notify_tx, &event_tx, &cursor, reason);
                    return CallbackResult::Keep;
                }
            }
            if drop_event {
                // Returning Drop should stop the event from being processed,
                // but Core Foundation still returns the event.
                cg_ev.set_type(CGEventType::Null);
                CallbackResult::Drop
            } else {
                CallbackResult::Keep
            }
        } else {
            CallbackResult::Keep
        }
    };

    let tap = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        cg_events_of_interest,
        event_tap_callback,
    )
    .map_err(|_| MacosCaptureCreationError::EventTapCreation)?;

    // Hand the mach port pointer to the callback so it can disable
    // capture when its queue or lock cannot keep up. The pointer is valid for the
    // lifetime of `tap` (which lives on the event-tap thread until
    // the run loop exits).
    let port_ptr = tap.mach_port().as_concrete_TypeRef() as usize;
    let _ = tap_mach_port.set(port_ptr);

    let tap_source: CFRunLoopSource = tap
        .mach_port()
        .create_runloop_source(0)
        .expect("Failed creating loop source");

    unsafe {
        CFRunLoop::get_current().add_source(&tap_source, kCFRunLoopCommonModes);
    }

    Ok(tap)
}

fn event_tap_thread(
    client_state: Arc<Mutex<InputCaptureState>>,
    cursor: CursorVisibility,
    event_tx: Arc<CaptureQueue>,
    notify_tx: Sender<ProducerEvent>,
    ready: std::sync::mpsc::Sender<Result<CFRunLoop, MacosCaptureCreationError>>,
    exit: oneshot::Sender<()>,
) {
    // Clone now: create_event_tap consumes notify_tx into its closure.
    let display_notify_tx = notify_tx.clone();

    let _tap = match create_event_tap(client_state, cursor, notify_tx, event_tx) {
        Err(e) => {
            ready.send(Err(e)).expect("channel closed");
            return;
        }
        Ok(tap) => {
            let run_loop = CFRunLoop::get_current();
            ready.send(Ok(run_loop)).expect("channel closed");
            tap
        }
    };

    // Register a Quartz display-reconfiguration callback so the
    // capture state's bounds get refreshed when the user plugs in a
    // monitor, changes resolution, or rearranges displays. The
    // callback runs on this thread's CFRunLoop. Box-leak the sender
    // so the C side has a stable user_info pointer; reclaim it after
    // the run loop exits.
    let display_user_info = Box::into_raw(Box::new(display_notify_tx)) as *mut c_void;
    unsafe {
        CGDisplayRegisterReconfigurationCallback(
            display_reconfiguration_callback,
            display_user_info,
        );
    }

    log::debug!("running CFRunLoop...");
    CFRunLoop::run_current();
    log::debug!("event tap thread exiting!...");

    unsafe {
        CGDisplayRemoveReconfigurationCallback(display_reconfiguration_callback, display_user_info);
        // Reclaim the leaked sender Box so we don't leak a tokio
        // channel sender on every capture create/destroy cycle.
        drop(Box::from_raw(
            display_user_info as *mut Sender<ProducerEvent>,
        ));
    }

    let _ = exit.send(());
}

/// Quartz display-reconfiguration callback. Fires twice per change:
/// once with `kCGDisplayBeginConfigurationFlag` set (BEFORE the
/// change is applied — the bounds are still stale at this point),
/// then again afterwards with the actual change flags (Add, Remove,
/// Mode, DesktopShapeChanged, etc.). Skip the begin phase; on the
/// real notification, kick the producer task to refresh bounds.
extern "C" fn display_reconfiguration_callback(_display: u32, flags: u32, user_info: *mut c_void) {
    const K_CG_DISPLAY_BEGIN_CONFIGURATION_FLAG: u32 = 1 << 0;
    if flags & K_CG_DISPLAY_BEGIN_CONFIGURATION_FLAG != 0 {
        return;
    }
    if user_info.is_null() {
        return;
    }
    // SAFETY: user_info is a Box::into_raw of Sender<ProducerEvent>
    // owned by `event_tap_thread`. It's valid for the lifetime of
    // that thread; the registration is removed before the box is
    // freed. The callback only fires while the run loop is running
    // on that thread, so we know the box is live here.
    let sender = unsafe { &*(user_info as *const Sender<ProducerEvent>) };
    if let Err(e) = sender.try_send(ProducerEvent::DisplayReconfigured) {
        log::warn!("failed to notify display reconfiguration: {e}");
    }
}

pub struct MacOSInputCapture {
    event_rx: Arc<CaptureQueue>,
    cursor: CursorVisibility,
    last_event_requires_enter_only: bool,
    run_loop: CFRunLoop,
    state: Arc<Mutex<InputCaptureState>>,
}

impl MacOSInputCapture {
    pub async fn new() -> Result<Self, MacosCaptureCreationError> {
        request_macos_capture_permissions()?;

        let (notify_tx, mut notify_rx) = mpsc::channel(32);
        let event_rx = Arc::new(CaptureQueue::default());
        let cursor_failure_events = event_rx.clone();
        let cursor_notify = notify_tx.clone();
        let cursor = CursorVisibility::new(
            |hidden| {
                if hidden {
                    CGDisplay::hide_cursor(&CGDisplay::main())
                } else {
                    CGDisplay::show_cursor(&CGDisplay::main())
                }
                .map_err(CaptureError::CoreGraphics)
            },
            move |error| {
                log::error!("cursor visibility worker failed: {error}");
                cursor_failure_events.fail();
                let _ = cursor_notify.try_send(ProducerEvent::EventTapDisabled);
            },
        );
        let state = Arc::new(Mutex::new(InputCaptureState::new(cursor.clone())?));
        let event_tx = event_rx.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (tap_exit_tx, mut tap_exit_rx) = oneshot::channel();

        unsafe {
            configure_cf_settings()?;
        }

        log::info!("Enabling CGEvent tap");
        let event_tap_thread_state = state.clone();
        let event_tap_notify = notify_tx.clone();
        let event_tap_cursor = cursor.clone();
        thread::spawn(move || {
            let queue = event_tx.clone();
            event_tap_thread(
                event_tap_thread_state,
                event_tap_cursor,
                event_tx,
                event_tap_notify,
                ready_tx,
                tap_exit_tx,
            );
            queue.close();
        });

        // wait for event tap creation result
        let run_loop = ready_rx.recv().expect("channel closed")?;

        let control_state = state.clone();
        let control_events = event_rx.clone();
        let _tap_task: tokio::task::JoinHandle<()> = tokio::task::spawn_local(async move {
            loop {
                tokio::select! {
                    producer_event = notify_rx.recv() => {
                        let Some(producer_event) = producer_event else {
                            break;
                        };
                        if let Err(e) = apply_producer_event(&state, producer_event).await {
                            log::error!("Failed to handle producer event: {e}");
                            control_events.fail();
                            break;
                        }
                    }
                    _ = &mut tap_exit_rx => break,
                }
            }
            let _ = apply_producer_event(&state, ProducerEvent::Release).await;
        });

        Ok(Self {
            event_rx,
            cursor,
            last_event_requires_enter_only: false,
            run_loop,
            state: control_state,
        })
    }
}

fn request_macos_capture_permissions() -> Result<(), MacosCaptureCreationError> {
    // Call both request functions unconditionally so macOS surfaces both
    // TCC prompts on the very first launch. TCC always returns `false` the
    // first time a permission is requested (the grant only becomes visible
    // on the next process launch), so returning early on the first failure
    // would skip the second prompt and force the user through an extra
    // relaunch just to see it.
    let accessibility = request_accessibility_permission();
    let input_monitoring = request_input_monitoring_permission();

    if !accessibility {
        return Err(MacosCaptureCreationError::AccessibilityPermission);
    }
    if !input_monitoring {
        return Err(MacosCaptureCreationError::InputMonitoringPermission);
    }
    Ok(())
}

fn request_accessibility_permission() -> bool {
    // Silent check. The GUI owns the one-time user-visible prompt at
    // startup (see lan_mouse_gtk::macos_privacy) so retries triggered by
    // clicking the "Reenable" button don't pop a fresh Accessibility
    // alert every time.
    unsafe { AXIsProcessTrusted() }
}

fn request_input_monitoring_permission() -> bool {
    // Silent check, same reasoning as above.
    unsafe { CGPreflightListenEventAccess() }
}

impl Drop for MacOSInputCapture {
    fn drop(&mut self) {
        let _ = self.cursor.set_hidden(false);
        self.run_loop.stop();
    }
}

#[async_trait]
impl Capture for MacOSInputCapture {
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError> {
        log::debug!("creating capture, {pos}");
        apply_producer_event(&self.state, ProducerEvent::Create(pos)).await?;
        log::debug!("done !");
        Ok(())
    }

    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError> {
        log::debug!("destroying capture {pos}");
        apply_producer_event(&self.state, ProducerEvent::Destroy(pos)).await?;
        log::debug!("done !");
        Ok(())
    }

    async fn set_enter_only(&mut self, pos: Position, enabled: bool) -> Result<(), CaptureError> {
        apply_producer_event(&self.state, ProducerEvent::SetEnterOnly(pos, enabled)).await
    }

    fn last_event_requires_enter_only(&self) -> bool {
        self.last_event_requires_enter_only
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        log::debug!("notifying Release");
        apply_producer_event(&self.state, ProducerEvent::Release).await
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        self.cursor.set_hidden(false)
    }
}

impl Stream for MacOSInputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.event_rx.poll_result(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error))),
            Poll::Ready(Some(Ok((pos, event, event_requires_enter_only)))) => {
                self.last_event_requires_enter_only = event_requires_enter_only;
                Poll::Ready(Some(Ok((pos, event))))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Bounds, CaptureError, CursorVisibility, Duration, InputCaptureState, InputSource, Mutex,
        Ordering, ProducerEvent,
    };
    use super::{CGEventType, Position, callback_lock, is_inward_motion, is_pointer_motion_event};

    #[test]
    fn failure_wakes_consumer_without_waiting_for_a_queue_lock() {
        use super::*;
        use std::sync::atomic::AtomicUsize;
        struct WakeCount(AtomicUsize);
        impl std::task::Wake for WakeCount {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let queue = CaptureQueue::default();
        let counter = Arc::new(WakeCount(AtomicUsize::new(0)));
        let waker = std::task::Waker::from(counter.clone());
        let mut cx = Context::from_waker(&waker);
        assert!(queue.poll_result(&mut cx).is_pending());
        let held = queue.events.lock().unwrap();
        queue.fail();
        assert_eq!(counter.0.load(Ordering::Relaxed), 1);
        assert!(matches!(
            queue.poll_result(&mut cx),
            Poll::Ready(Some(Err(CaptureError::EventTapDisabled)))
        ));
        drop(held);
    }

    #[test]
    fn failure_precedes_buffered_input_and_does_not_require_another_event() {
        use super::*;
        let queue = CaptureQueue::default();
        queue
            .push((Position::Left, CaptureEvent::Begin, false))
            .unwrap();
        queue.fail();
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(
            queue.poll_result(&mut cx),
            Poll::Ready(Some(Err(CaptureError::EventTapDisabled)))
        ));
    }

    #[test]
    fn full_control_channel_cannot_hide_failure_or_keep_cursor_hidden() {
        use super::*;
        let (shown_tx, shown_rx) = std::sync::mpsc::channel();
        let (hidden_tx, hidden_rx) = std::sync::mpsc::channel();
        let cursor = CursorVisibility::new(
            move |hidden| {
                if hidden {
                    hidden_tx.send(()).unwrap();
                } else {
                    shown_tx.send(()).unwrap();
                }
                Ok(())
            },
            |e| panic!("unexpected cursor error: {e}"),
        );
        cursor.set_hidden(true).unwrap();
        hidden_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let (notify, _rx) = mpsc::channel(1);
        notify.try_send(ProducerEvent::Release).unwrap();
        let queue = CaptureQueue::default();
        record_capture_failure(&notify, &queue, &cursor);
        shown_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(queue.failed.load(Ordering::Acquire));
    }

    #[test]
    fn over_budget_callback_latches_failure_inside_program() {
        use super::*;
        let cursor = CursorVisibility::new(|_| Ok(()), |_| {});
        let (notify, _rx) = mpsc::channel(1);
        let events = CaptureQueue::default();
        let port = OnceLock::new(); // No real event tap is needed for this test.
        drop(CallbackTiming {
            started: Instant::now() - Duration::from_secs(2),
            port: &port,
            notify: &notify,
            events: &events,
            cursor: &cursor,
        });
        assert!(events.failed.load(Ordering::Acquire));
    }

    #[test]
    fn concurrent_motion_producer_and_consumer_preserve_distance() {
        use super::*;
        let queue = Arc::new(CaptureQueue::default());
        let producer_queue = queue.clone();
        let producer = thread::spawn(move || {
            let result = (|| {
                for _ in 0..10000 {
                    producer_queue.push((
                        Position::Left,
                        CaptureEvent::Input(Event::Pointer(PointerEvent::Motion {
                            time: 0,
                            dx: 1.0,
                            dy: -1.0,
                        })),
                        false,
                    ))?;
                }
                Ok::<(), &'static str>(())
            })();
            producer_queue.close();
            result
        });
        let w = futures::task::noop_waker();
        let mut cx = Context::from_waker(&w);
        let (mut total_x, mut total_y) = (0.0, 0.0);
        loop {
            match queue.poll(&mut cx) {
                Poll::Ready(Some((
                    _,
                    CaptureEvent::Input(Event::Pointer(PointerEvent::Motion { dx, dy, .. })),
                    _,
                ))) => {
                    total_x += dx;
                    total_y += dy;
                }
                Poll::Ready(None) => break,
                Poll::Pending => thread::yield_now(),
                _ => panic!("unexpected event"),
            }
        }
        producer.join().unwrap().unwrap();
        assert_eq!((total_x, total_y), (10000.0, -10000.0));
    }

    #[test]
    fn queue_lock_deadline_is_bounded_and_recovers() {
        use super::*;
        let queue = CaptureQueue::default();
        let held = queue.events.lock().unwrap();
        assert_eq!(
            queue.push((Position::Left, CaptureEvent::Begin, false)),
            Err("capture queue lock deadline exceeded")
        );
        drop(held);
        assert!(
            queue
                .push((Position::Left, CaptureEvent::Begin, false))
                .is_ok()
        );
    }

    #[test]
    fn motion_queue_preserves_distance_and_button_order() {
        use super::*;
        let queue = CaptureQueue::default();
        let motion = |dx| {
            (
                Position::Left,
                CaptureEvent::Input(Event::Pointer(PointerEvent::Motion {
                    time: 0,
                    dx,
                    dy: 1.0,
                })),
                false,
            )
        };
        for _ in 0..1000 {
            queue.push(motion(0.5)).unwrap();
        }
        let button = (
            Position::Left,
            CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_LEFT,
                state: 1,
            })),
            false,
        );
        queue.push(button).unwrap();
        queue.push(motion(-1.0)).unwrap();
        let data = queue.events.lock().unwrap();
        assert_eq!(data.0.len(), 3);
        assert_eq!(
            data.0[0],
            (
                Position::Left,
                CaptureEvent::Input(Event::Pointer(PointerEvent::Motion {
                    time: 0,
                    dx: 500.0,
                    dy: 1000.0
                })),
                false
            )
        );
        assert_eq!(data.0[1], button);
        assert_eq!(data.0[2], motion(-1.0));
    }

    #[test]
    fn motion_queue_remains_bounded_and_wakes_consumer() {
        use super::*;
        use futures::task::{ArcWake, waker};
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct WakeCount(AtomicUsize);
        impl ArcWake for WakeCount {
            fn wake_by_ref(this: &Arc<Self>) {
                this.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let count = Arc::new(WakeCount(AtomicUsize::new(0)));
        let w = waker(count.clone());
        let mut cx = Context::from_waker(&w);
        let queue = CaptureQueue::default();
        assert!(queue.poll(&mut cx).is_pending());
        let event = (Position::Left, CaptureEvent::Begin, false);
        for _ in 0..32 {
            queue.push(event).unwrap();
        }
        assert!(count.0.load(Ordering::SeqCst) > 0);
        assert!(queue.push(event).is_err());
        for _ in 0..32 {
            assert_eq!(queue.poll(&mut cx), Poll::Ready(Some(event)));
        }
        queue.close();
        assert_eq!(queue.poll(&mut cx), Poll::Ready(None));
        assert!(queue.push(event).is_err());
    }

    #[test]
    fn callback_does_not_wait_for_an_unavailable_consumer_lock() {
        let state = tokio::sync::Mutex::new(42);
        let held = state.try_lock().unwrap();
        assert!(callback_lock(&state).is_none());
        drop(held);
        assert_eq!(*callback_lock(&state).unwrap(), 42);
    }

    #[test]
    fn slow_cursor_operation_does_not_hold_capture_state_lock() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let (shown_tx, shown_rx) = std::sync::mpsc::channel();
        let cursor = CursorVisibility::new(
            move |hidden| {
                if hidden {
                    started_tx.send(()).unwrap();
                    resume_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                } else {
                    shown_tx.send(()).unwrap();
                }
                Ok(())
            },
            |error| panic!("unexpected cursor error: {error}"),
        );
        let state = Mutex::new(InputCaptureState::with_bounds(cursor, Bounds::default()));
        callback_lock(&state)
            .unwrap()
            .handle_producer_event(ProducerEvent::Grab(Position::Left, InputSource::Physical))
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // The native operation is still blocked, but capture state is available.
        let mut guard = callback_lock(&state).expect("cursor operation must not retain state lock");
        assert_eq!(guard.current_pos, Some(Position::Left));
        for _ in 0..1000 {
            guard.cursor.set_hidden(true).unwrap();
        }
        guard.handle_producer_event(ProducerEvent::Release).unwrap();
        assert_eq!(guard.current_pos, None);
        drop(guard);
        resume_tx.send(()).unwrap();
        shown_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn cursor_worker_restores_visibility_on_shutdown() {
        let (tx, rx) = std::sync::mpsc::channel();
        let cursor = CursorVisibility::new(
            move |hidden| {
                tx.send(hidden).unwrap();
                Ok(())
            },
            |error| panic!("unexpected cursor error: {error}"),
        );
        cursor.set_hidden(true).unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        drop(cursor);
        assert!(!rx.recv_timeout(Duration::from_secs(1)).unwrap());
    }

    #[test]
    fn failed_cursor_worker_rejects_new_capture_requests() {
        let (tx, rx) = std::sync::mpsc::channel();
        let cursor = CursorVisibility::new(
            |_| Err(std::io::Error::other("simulated cursor failure").into()),
            move |_| {
                tx.send(()).unwrap();
            },
        );
        cursor.set_hidden(true).unwrap();
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(cursor.failed.load(Ordering::Acquire));
        assert!(matches!(
            cursor.set_hidden(true),
            Err(CaptureError::EventTapDisabled)
        ));
    }

    #[test]
    fn even_short_inward_motion_rearms_each_edge() {
        assert!(is_inward_motion(Position::Left, 0.5, 0.0));
        assert!(is_inward_motion(Position::Right, -0.5, 0.0));
        assert!(is_inward_motion(Position::Top, 0.0, 0.5));
        assert!(is_inward_motion(Position::Bottom, 0.0, -0.5));

        assert!(!is_inward_motion(Position::Left, -1.0, 0.0));
        assert!(!is_inward_motion(Position::Right, 1.0, 0.0));
        assert!(!is_inward_motion(Position::Top, 0.0, -1.0));
        assert!(!is_inward_motion(Position::Bottom, 0.0, 1.0));
        assert!(!is_inward_motion(Position::Left, 0.0, 0.0));
    }

    #[test]
    fn dragged_pointer_events_are_motion_events() {
        assert!(is_pointer_motion_event(CGEventType::MouseMoved));
        assert!(is_pointer_motion_event(CGEventType::LeftMouseDragged));
        assert!(is_pointer_motion_event(CGEventType::RightMouseDragged));
        assert!(is_pointer_motion_event(CGEventType::OtherMouseDragged));
        assert!(!is_pointer_motion_event(CGEventType::LeftMouseDown));
    }
}

type CGSConnectionID = u32;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGSSetConnectionProperty(
        cid: CGSConnectionID,
        targetCID: CGSConnectionID,
        key: CFStringRef,
        value: CFBooleanRef,
    ) -> CGError;
    fn _CGSDefaultConnection() -> CGSConnectionID;
}

extern "C" {
    fn CGEventSourceSetLocalEventsSuppressionInterval(
        event_source: CGEventSource,
        seconds: CFTimeInterval,
    );
    fn CGPreflightListenEventAccess() -> bool;
    /// Re-enable an event tap that was disabled by a
    /// `kCGEventTapDisabledByTimeout` event. The Apple-documented
    /// recovery path: see Quartz Event Services Reference. The `tap`
    /// argument is a `CFMachPortRef`; we pass the raw pointer so we
    /// can store it as `usize` for cross-thread sharing.
    fn CGEventTapEnable(tap: *mut c_void, enable: bool);

    /// Register a callback invoked when the display configuration
    /// changes (monitor add/remove, resolution change, mirror,
    /// rearrange, etc). See Quartz Display Services Reference.
    fn CGDisplayRegisterReconfigurationCallback(
        callback: extern "C" fn(u32, u32, *mut c_void),
        user_info: *mut c_void,
    ) -> CGError;
    fn CGDisplayRemoveReconfigurationCallback(
        callback: extern "C" fn(u32, u32, *mut c_void),
        user_info: *mut c_void,
    ) -> CGError;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

unsafe fn configure_cf_settings() -> Result<(), MacosCaptureCreationError> {
    // When we warp the cursor using CGWarpMouseCursorPosition local events are suppressed for a short time
    // this leeds to the cursor not flowing when crossing back from a clinet, set this to to 0 stops the warp
    // from working, set a low value by trial and error, 0.05s seems good. 0.25s is the default
    let event_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .map_err(|_| MacosCaptureCreationError::EventSourceCreation)?;
    CGEventSourceSetLocalEventsSuppressionInterval(event_source, 0.05);
    // FIXME Memory Leak

    // This is a private settings that allows the cursor to be hidden while in the background.
    // It is used by Barrier and other apps.
    let key = CString::new("SetsCursorInBackground").unwrap();
    let cf_key = CFStringCreateWithCString(
        kCFAllocatorDefault,
        key.as_ptr() as *const c_char,
        kCFStringEncodingUTF8,
    );
    if CGSSetConnectionProperty(
        _CGSDefaultConnection(),
        _CGSDefaultConnection(),
        cf_key,
        kCFBooleanTrue,
    ) != kCGErrorSuccess
    {
        return Err(MacosCaptureCreationError::CGCursorProperty);
    }
    CFRelease(cf_key as *const c_void);
    Ok(())
}

// From X11/X.h
bitflags! {
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
    struct XMods: u32 {
        const ShiftMask = (1<<0);
        const LockMask = (1<<1);
        const ControlMask = (1<<2);
        const Mod1Mask = (1<<3);
        const Mod2Mask = (1<<4);
        const Mod3Mask = (1<<5);
        const Mod4Mask = (1<<6);
        const Mod5Mask = (1<<7);
    }
}
