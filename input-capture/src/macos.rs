use super::{Capture, CaptureError, CaptureEvent, Position, error::MacosCaptureCreationError};
use async_trait::async_trait;
use bitflags::bitflags;
use core_foundation::{
    base::{CFRelease, kCFAllocatorDefault},
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
use input_event::{BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Event, KeyboardEvent, PointerEvent};
use keycode::{KeyMap, KeyMapping};
use libc::c_void;
use once_cell::unsync::Lazy;
use std::{
    collections::HashSet,
    ffi::{CString, c_char},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, ready},
    thread::{self},
};
use tokio::sync::{
    Mutex,
    mpsc::{self, Receiver, Sender},
    oneshot,
};

#[derive(Debug, Default)]
struct Bounds {
    xmin: f64,
    xmax: f64,
    ymin: f64,
    ymax: f64,
}

#[derive(Debug)]
struct InputCaptureState {
    /// active capture positions
    active_clients: Lazy<HashSet<Position>>,
    /// the currently entered capture position, if any
    current_pos: Option<Position>,
    /// position where the cursor was captured
    enter_position: Option<CGPoint>,
    /// bounds of the input capture area
    bounds: Bounds,
    /// current state of modifier keys
    modifier_state: XMods,
}

#[derive(Debug)]
enum ProducerEvent {
    Release,
    Create(Position),
    Destroy(Position),
    Grab(Position),
    EventTapDisabled,
}

impl InputCaptureState {
    fn new() -> Result<Self, MacosCaptureCreationError> {
        let mut res = Self {
            active_clients: Lazy::new(HashSet::new),
            current_pos: None,
            enter_position: None,
            bounds: Bounds::default(),
            modifier_state: Default::default(),
        };
        res.update_bounds()?;
        Ok(res)
    }

    fn crossed(&mut self, event: &CGEvent) -> Option<Position> {
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
    fn update_bounds(&mut self) -> Result<(), MacosCaptureCreationError> {
        let active_ids =
            CGDisplay::active_displays().map_err(MacosCaptureCreationError::ActiveDisplays)?;
        active_ids.iter().for_each(|d| {
            let bounds = CGDisplay::new(*d).bounds();
            self.bounds.xmin = self.bounds.xmin.min(bounds.origin.x);
            self.bounds.xmax = self.bounds.xmax.max(bounds.origin.x + bounds.size.width);
            self.bounds.ymin = self.bounds.ymin.min(bounds.origin.y);
            self.bounds.ymax = self.bounds.ymax.max(bounds.origin.y + bounds.size.height);
        });

        log::debug!("Updated displays bounds: {0:?}", self.bounds);
        Ok(())
    }

    /// start the input capture by
    fn start_capture(&mut self, event: &CGEvent, position: Position) -> Result<(), CaptureError> {
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
        self.reset_cursor()
    }

    /// resets the cursor to the position, where the capture started
    fn reset_cursor(&mut self) -> Result<(), CaptureError> {
        let pos = self.enter_position.expect("capture active");
        log::trace!("Resetting cursor position to: {}, {}", pos.x, pos.y);
        CGDisplay::warp_mouse_cursor_position(pos).map_err(CaptureError::WarpCursor)
    }

    fn hide_cursor(&self) -> Result<(), CaptureError> {
        CGDisplay::hide_cursor(&CGDisplay::main()).map_err(CaptureError::CoreGraphics)
    }

    fn show_cursor(&self) -> Result<(), CaptureError> {
        CGDisplay::show_cursor(&CGDisplay::main()).map_err(CaptureError::CoreGraphics)
    }

    async fn handle_producer_event(
        &mut self,
        producer_event: ProducerEvent,
    ) -> Result<(), CaptureError> {
        log::debug!("handling event: {producer_event:?}");
        match producer_event {
            ProducerEvent::Release => {
                if self.current_pos.is_some() {
                    self.show_cursor()?;
                    self.current_pos = None;
                }
            }
            ProducerEvent::Grab(pos) => {
                if self.current_pos.is_none() {
                    self.hide_cursor()?;
                    self.current_pos = Some(pos);
                }
            }
            ProducerEvent::Create(p) => {
                self.active_clients.insert(p);
            }
            ProducerEvent::Destroy(p) => {
                if let Some(current) = self.current_pos {
                    if current == p {
                        self.show_cursor()?;
                        self.current_pos = None;
                    };
                }
                self.active_clients.remove(&p);
            }
            ProducerEvent::EventTapDisabled => return Err(CaptureError::EventTapDisabled),
        };
        Ok(())
    }
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
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_MIDDLE,
                state: 1,
            })))
        }
        CGEventType::OtherMouseUp => {
            result.push(CaptureEvent::Input(Event::Pointer(PointerEvent::Button {
                time: 0,
                button: BTN_MIDDLE,
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
    notify_tx: Sender<ProducerEvent>,
    event_tx: Sender<(Position, CaptureEvent)>,
) -> Result<CGEventTap<'a>, MacosCaptureCreationError> {
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

    let event_tap_callback =
        move |_proxy: CGEventTapProxy, event_type: CGEventType, cg_ev: &CGEvent| {
            log::trace!("Got event from tap: {event_type:?}");
            let mut state = client_state.blocking_lock();
            let mut capture_position = None;
            let mut res_events = vec![];

            if matches!(
                event_type,
                CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
            ) {
                log::error!("CGEventTap disabled");
                notify_tx
                    .blocking_send(ProducerEvent::EventTapDisabled)
                    .unwrap_or_else(|e| {
                        log::error!("Failed to send notification: {e}");
                    });
            }

            // Are we in a client?
            if let Some(current_pos) = state.current_pos {
                capture_position = Some(current_pos);
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
                if matches!(
                    event_type,
                    CGEventType::MouseMoved
                        | CGEventType::LeftMouseDragged
                        | CGEventType::RightMouseDragged
                        | CGEventType::OtherMouseDragged
                ) {
                    state.reset_cursor().unwrap_or_else(|e| log::warn!("{e}"));
                }
            } else if matches!(event_type, CGEventType::MouseMoved) {
                // Did we cross a barrier?
                if let Some(new_pos) = state.crossed(cg_ev) {
                    capture_position = Some(new_pos);
                    state
                        .start_capture(cg_ev, new_pos)
                        .unwrap_or_else(|e| log::warn!("{e}"));
                    res_events.push(CaptureEvent::Begin);
                    notify_tx
                        .blocking_send(ProducerEvent::Grab(new_pos))
                        .expect("Failed to send notification");
                }
            }

            if let Some(pos) = capture_position {
                res_events.iter().for_each(|e| {
                    // error must be ignored, since the event channel
                    // may already be closed when the InputCapture instance is dropped.
                    let _ = event_tx.blocking_send((pos, *e));
                });
                // Returning Drop should stop the event from being processed
                // but core fundation still returns the event
                cg_ev.set_type(CGEventType::Null);
            }
            CallbackResult::Replace(cg_ev.to_owned())
        };

    let tap = CGEventTap::new(
        CGEventTapLocation::Session,
        CGEventTapPlacement::HeadInsertEventTap,
        CGEventTapOptions::Default,
        cg_events_of_interest,
        event_tap_callback,
    )
    .map_err(|_| MacosCaptureCreationError::EventTapCreation)?;

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
    event_tx: Sender<(Position, CaptureEvent)>,
    notify_tx: Sender<ProducerEvent>,
    ready: std::sync::mpsc::Sender<Result<CFRunLoop, MacosCaptureCreationError>>,
    exit: oneshot::Sender<()>,
) {
    let _tap = match create_event_tap(client_state, notify_tx, event_tx) {
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
    log::debug!("running CFRunLoop...");
    CFRunLoop::run_current();
    log::debug!("event tap thread exiting!...");

    let _ = exit.send(());
}

pub struct MacOSInputCapture {
    event_rx: Receiver<(Position, CaptureEvent)>,
    notify_tx: Sender<ProducerEvent>,
    run_loop: CFRunLoop,
}

impl MacOSInputCapture {
    pub async fn new() -> Result<Self, MacosCaptureCreationError> {
        let state = Arc::new(Mutex::new(InputCaptureState::new()?));
        let (event_tx, event_rx) = mpsc::channel(32);
        let (notify_tx, mut notify_rx) = mpsc::channel(32);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (tap_exit_tx, mut tap_exit_rx) = oneshot::channel();

        unsafe {
            configure_cf_settings()?;
        }

        log::info!("Enabling CGEvent tap");
        let event_tap_thread_state = state.clone();
        let event_tap_notify = notify_tx.clone();
        thread::spawn(move || {
            event_tap_thread(
                event_tap_thread_state,
                event_tx,
                event_tap_notify,
                ready_tx,
                tap_exit_tx,
            )
        });

        // wait for event tap creation result
        let run_loop = ready_rx.recv().expect("channel closed")?;

        let _tap_task: tokio::task::JoinHandle<()> = tokio::task::spawn_local(async move {
            loop {
                tokio::select! {
                    producer_event = notify_rx.recv() => {
                        let Some(producer_event) = producer_event else {
                            break;
                        };
                        let mut state = state.lock().await;
                        state.handle_producer_event(producer_event).await.unwrap_or_else(|e| {
                            log::error!("Failed to handle producer event: {e}");
                        })
                    }
                    _ = &mut tap_exit_rx => break,
                }
            }
            // show cursor
            let _ = CGDisplay::show_cursor(&CGDisplay::main());
        });

        Ok(Self {
            event_rx,
            notify_tx,
            run_loop,
        })
    }
}

impl Drop for MacOSInputCapture {
    fn drop(&mut self) {
        self.run_loop.stop();
    }
}

#[async_trait]
impl Capture for MacOSInputCapture {
    async fn create(&mut self, pos: Position) -> Result<(), CaptureError> {
        let notify_tx = self.notify_tx.clone();
        tokio::task::spawn_local(async move {
            log::debug!("creating capture, {pos}");
            let _ = notify_tx.send(ProducerEvent::Create(pos)).await;
            log::debug!("done !");
        });
        Ok(())
    }

    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError> {
        let notify_tx = self.notify_tx.clone();
        tokio::task::spawn_local(async move {
            log::debug!("destroying capture {pos}");
            let _ = notify_tx.send(ProducerEvent::Destroy(pos)).await;
            log::debug!("done !");
        });
        Ok(())
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        let notify_tx = self.notify_tx.clone();
        tokio::task::spawn_local(async move {
            log::debug!("notifying Release");
            let _ = notify_tx.send(ProducerEvent::Release).await;
        });
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        Ok(())
    }
}

impl Stream for MacOSInputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match ready!(self.event_rx.poll_recv(cx)) {
            None => Poll::Ready(None),
            Some(e) => Poll::Ready(Some(Ok(e))),
        }
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
}

unsafe fn configure_cf_settings() -> Result<(), MacosCaptureCreationError> {
    // When we warp the cursor using CGWarpMouseCursorPosition local events are suppressed for a short time
    // this leeds to the cursor not flowing when crossing back from a clinet, set this to to 0 stops the warp
    // from working, set a low value by trial and error, 0.05s seems good. 0.25s is the default
    let event_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .map_err(|_| MacosCaptureCreationError::EventSourceCreation)?;
    CGEventSourceSetLocalEventsSuppressionInterval(event_source, 0.05);

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
