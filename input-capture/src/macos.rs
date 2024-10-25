use super::{error::MacosCaptureCreationError, Capture, CaptureError, CaptureEvent, Position};
use async_trait::async_trait;
use bitflags::bitflags;
use core_foundation::base::{kCFAllocatorDefault, CFRelease};
use core_foundation::date::CFTimeInterval;
use core_foundation::number::{kCFBooleanTrue, CFBooleanRef};
use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop, CFRunLoopSource};
use core_foundation::string::{kCFStringEncodingUTF8, CFStringCreateWithCString, CFStringRef};
use core_graphics::base::{kCGErrorSuccess, CGError};
use core_graphics::display::{CGDisplay, CGPoint};
use core_graphics::event::{
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, EventField,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use futures_core::Stream;
use input_event::{Event, KeyboardEvent, PointerEvent, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT};
use keycode::{KeyMap, KeyMapping};
use libc::c_void;
use once_cell::unsync::Lazy;
use std::collections::HashSet;
use std::ffi::{c_char, CString};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{ready, Context, Poll};
use std::thread::{self};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::{oneshot, Mutex};

#[derive(Debug, Default)]
struct Bounds {
    xmin: f64,
    xmax: f64,
    ymin: f64,
    ymax: f64,
}

#[derive(Debug)]
struct InputCaptureState {
    active_clients: Lazy<HashSet<Position>>,
    current_pos: Option<Position>,
    bounds: Bounds,
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
            bounds: Bounds::default(),
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

    // We can't disable mouse movement when in a client so we need to reset the cursor position
    // to the edge of the screen, the cursor will be hidden but we dont want it to appear in a
    // random location when we exit the client
    fn reset_mouse_position(&self, event: &CGEvent) -> Result<(), CaptureError> {
        if let Some(pos) = self.current_pos {
            let location = event.location();
            let edge_offset = 1.0;

            // After the cursor is warped no event is produced but the next event
            // will carry the delta from the warp so only half the delta is needed to move the cursor
            let delta_y = event.get_double_value_field(EventField::MOUSE_EVENT_DELTA_Y) / 2.0;
            let delta_x = event.get_double_value_field(EventField::MOUSE_EVENT_DELTA_X) / 2.0;

            let mut new_x = location.x + delta_x;
            let mut new_y = location.y + delta_y;

            match pos {
                Position::Left => {
                    new_x = self.bounds.xmin + edge_offset;
                }
                Position::Right => {
                    new_x = self.bounds.xmax - edge_offset;
                }
                Position::Top => {
                    new_y = self.bounds.ymin + edge_offset;
                }
                Position::Bottom => {
                    new_y = self.bounds.ymax - edge_offset;
                }
            }
            let new_pos = CGPoint::new(new_x, new_y);

            log::trace!("Resetting cursor position to: {new_x}, {new_y}");

            return CGDisplay::warp_mouse_cursor_position(new_pos)
                .map_err(CaptureError::WarpCursor);
        }

        Err(CaptureError::ResetMouseWithoutClient)
    }

    async fn handle_producer_event(
        &mut self,
        producer_event: ProducerEvent,
    ) -> Result<(), CaptureError> {
        log::debug!("handling event: {producer_event:?}");
        match producer_event {
            ProducerEvent::Release => {
                if self.current_pos.is_some() {
                    CGDisplay::show_cursor(&CGDisplay::main())
                        .map_err(CaptureError::CoreGraphics)?;
                    self.current_pos = None;
                }
            }
            ProducerEvent::Grab(pos) => {
                if self.current_pos.is_none() {
                    CGDisplay::hide_cursor(&CGDisplay::main())
                        .map_err(CaptureError::CoreGraphics)?;
                    self.current_pos = Some(pos);
                }
            }
            ProducerEvent::Create(p) => {
                self.active_clients.insert(p);
            }
            ProducerEvent::Destroy(p) => {
                if let Some(current) = self.current_pos {
                    if current == p {
                        CGDisplay::show_cursor(&CGDisplay::main())
                            .map_err(CaptureError::CoreGraphics)?;
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
            let mut mods = XMods::empty();
            let mut mods_locked = XMods::empty();
            let cg_flags = ev.get_flags();

            if cg_flags.contains(CGEventFlags::CGEventFlagShift) {
                mods |= XMods::ShiftMask;
            }
            if cg_flags.contains(CGEventFlags::CGEventFlagControl) {
                mods |= XMods::ControlMask;
            }
            if cg_flags.contains(CGEventFlags::CGEventFlagAlternate) {
                mods |= XMods::Mod1Mask;
            }
            if cg_flags.contains(CGEventFlags::CGEventFlagCommand) {
                mods |= XMods::Mod4Mask;
            }
            if cg_flags.contains(CGEventFlags::CGEventFlagAlphaShift) {
                mods |= XMods::LockMask;
                mods_locked |= XMods::LockMask;
            }

            let modifier_event = KeyboardEvent::Modifiers {
                depressed: mods.bits(),
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
            let v = ev.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_1);
            let h = ev.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_POINT_DELTA_AXIS_2);
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
            let mut pos = None;
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
                pos = Some(current_pos);
                get_events(&event_type, cg_ev, &mut res_events).unwrap_or_else(|e| {
                    log::error!("Failed to get events: {e}");
                });

                // Keep (hidden) cursor at the edge of the screen
                if matches!(event_type, CGEventType::MouseMoved) {
                    state.reset_mouse_position(cg_ev).unwrap_or_else(|e| {
                        log::error!("Failed to reset mouse position: {e}");
                    })
                }
            }
            // Did we cross a barrier?
            else if matches!(event_type, CGEventType::MouseMoved) {
                if let Some(new_pos) = state.crossed(cg_ev) {
                    pos = Some(new_pos);
                    res_events.push(CaptureEvent::Begin);
                    notify_tx
                        .blocking_send(ProducerEvent::Grab(new_pos))
                        .expect("Failed to send notification");
                }
            }

            if let Some(pos) = pos {
                res_events.iter().for_each(|e| {
                    event_tx
                        .blocking_send((pos, *e))
                        .expect("Failed to send event");
                });
                // Returning None should stop the event from being processed
                // but core fundation still returns the event
                cg_ev.set_type(CGEventType::Null);
            }
            Some(cg_ev.to_owned())
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
        .mach_port
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
    ready: std::sync::mpsc::Sender<Result<(), MacosCaptureCreationError>>,
    exit: oneshot::Sender<Result<(), &'static str>>,
) {
    let _tap = match create_event_tap(client_state, notify_tx, event_tx) {
        Err(e) => {
            ready.send(Err(e)).expect("channel closed");
            return;
        }
        Ok(tap) => {
            ready.send(Ok(())).expect("channel closed");
            tap
        }
    };
    CFRunLoop::run_current();

    let _ = exit.send(Err("tap thread exited"));
}

pub struct MacOSInputCapture {
    event_rx: Receiver<(Position, CaptureEvent)>,
    notify_tx: Sender<ProducerEvent>,
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
        ready_rx.recv().expect("channel closed")?;

        let _tap_task: tokio::task::JoinHandle<()> = tokio::task::spawn_local(async move {
            loop {
                tokio::select! {
                    producer_event = notify_rx.recv() => {
                        let producer_event = producer_event.expect("channel closed");
                        let mut state = state.lock().await;
                        state.handle_producer_event(producer_event).await.unwrap_or_else(|e| {
                            log::error!("Failed to handle producer event: {e}");
                        })
                    }

                    res = &mut tap_exit_rx => {
                        if let Err(e) = res.expect("channel closed") {
                            log::error!("Tap thread failed: {:?}", e);
                            break;
                        }
                    }
                }
            }
        });

        Ok(Self {
            event_rx,
            notify_tx,
        })
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
