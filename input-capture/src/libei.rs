use ashpd::{
    desktop::{
        input_capture::{Activated, Barrier, BarrierID, Capabilities, InputCapture, Region, Zones},
        ResponseError, Session,
    },
    enumflags2::BitFlags,
};
use async_trait::async_trait;
use futures::{FutureExt, StreamExt};
use reis::{
    ei::{self, keyboard::KeyState},
    eis::button::ButtonState,
    event::{DeviceCapability, EiEvent},
    tokio::{EiConvertEventStream, EiEventStream},
};
use std::{
    cell::Cell,
    collections::HashMap,
    io,
    os::unix::net::UnixStream,
    pin::Pin,
    rc::Rc,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    sync::{
        mpsc::{self, Receiver, Sender},
        Notify,
    },
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use futures_core::Stream;
use once_cell::sync::Lazy;

use input_event::{Event, KeyboardEvent, PointerEvent};

use super::{
    error::{CaptureError, LibeiCaptureCreationError, ReisConvertEventStreamError},
    CaptureHandle, InputCapture as LanMouseInputCapture, Position,
};

/* there is a bug in xdg-remote-desktop-portal-gnome / mutter that
 * prevents receiving further events after a session has been disabled once.
 * Therefore the session needs to recreated when the barriers are updated */

/// events that necessitate restarting the capture session
#[derive(Clone, Copy, Debug)]
enum LibeiNotifyEvent {
    Create(CaptureHandle, Position),
    Destroy(CaptureHandle),
}

#[allow(dead_code)]
pub struct LibeiInputCapture<'a> {
    input_capture: Pin<Box<InputCapture<'a>>>,
    capture_task: JoinHandle<Result<(), CaptureError>>,
    event_rx: Receiver<(CaptureHandle, Event)>,
    notify_capture: Sender<LibeiNotifyEvent>,
    notify_release: Arc<Notify>,
    cancellation_token: CancellationToken,
}

static INTERFACES: Lazy<HashMap<&'static str, u32>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("ei_connection", 1);
    m.insert("ei_callback", 1);
    m.insert("ei_pingpong", 1);
    m.insert("ei_seat", 1);
    m.insert("ei_device", 2);
    m.insert("ei_pointer", 1);
    m.insert("ei_pointer_absolute", 1);
    m.insert("ei_scroll", 1);
    m.insert("ei_button", 1);
    m.insert("ei_keyboard", 1);
    m.insert("ei_touchscreen", 1);
    m
});

fn pos_to_barrier(r: &Region, pos: Position) -> (i32, i32, i32, i32) {
    let (x, y) = (r.x_offset(), r.y_offset());
    let (width, height) = (r.width() as i32, r.height() as i32);
    match pos {
        Position::Left => (x, y, x, y + height - 1), // start pos, end pos, inclusive
        Position::Right => (x + width, y, x + width, y + height - 1),
        Position::Top => (x, y, x + width - 1, y),
        Position::Bottom => (x, y + height, x + width - 1, y + height),
    }
}

fn select_barriers(
    zones: &Zones,
    clients: &Vec<(CaptureHandle, Position)>,
    next_barrier_id: &mut u32,
) -> (Vec<Barrier>, HashMap<BarrierID, CaptureHandle>) {
    let mut client_for_barrier = HashMap::new();
    let mut barriers: Vec<Barrier> = vec![];

    for (handle, pos) in clients {
        let mut client_barriers = zones
            .regions()
            .iter()
            .map(|r| {
                let id = *next_barrier_id;
                *next_barrier_id = id + 1;
                let position = pos_to_barrier(r, *pos);
                client_for_barrier.insert(id, *handle);
                Barrier::new(id, position)
            })
            .collect();
        barriers.append(&mut client_barriers);
    }
    (barriers, client_for_barrier)
}

async fn update_barriers(
    input_capture: &InputCapture<'_>,
    session: &Session<'_>,
    active_clients: &Vec<(CaptureHandle, Position)>,
    next_barrier_id: &mut u32,
) -> Result<HashMap<BarrierID, CaptureHandle>, ashpd::Error> {
    let zones = input_capture.zones(session).await?.response()?;
    log::debug!("zones: {zones:?}");

    let (barriers, id_map) = select_barriers(&zones, active_clients, next_barrier_id);
    log::debug!("barriers: {barriers:?}");
    log::debug!("client for barrier id: {id_map:?}");

    let response = input_capture
        .set_pointer_barriers(session, &barriers, zones.zone_set())
        .await?;
    let response = response.response()?;
    log::debug!("{response:?}");
    Ok(id_map)
}

async fn create_session<'a>(
    input_capture: &'a InputCapture<'a>,
) -> std::result::Result<(Session<'a>, BitFlags<Capabilities>), ashpd::Error> {
    log::debug!("creating input capture session");
    let (session, capabilities) = loop {
        match input_capture
            .create_session(
                &ashpd::WindowIdentifier::default(),
                Capabilities::Keyboard | Capabilities::Pointer | Capabilities::Touchscreen,
            )
            .await
        {
            Ok(s) => break s,
            Err(ashpd::Error::Response(ResponseError::Cancelled)) => continue,
            o => o?,
        };
    };
    log::debug!("capabilities: {capabilities:?}");
    Ok((session, capabilities))
}

async fn connect_to_eis(
    input_capture: &InputCapture<'_>,
    session: &Session<'_>,
) -> Result<(ei::Context, EiConvertEventStream), CaptureError> {
    log::debug!("connect_to_eis");
    let fd = input_capture.connect_to_eis(session).await?;

    // create unix stream from fd
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(true)?;

    // create ei context
    let context = ei::Context::new(stream)?;
    let mut event_stream = EiEventStream::new(context.clone())?;
    let response = reis::tokio::ei_handshake(
        &mut event_stream,
        "de.feschber.LanMouse",
        ei::handshake::ContextType::Receiver,
        &INTERFACES,
    )
    .await?;
    let event_stream = EiConvertEventStream::new(event_stream, response.serial);

    Ok((context, event_stream))
}

async fn libei_event_handler(
    mut ei_event_stream: EiConvertEventStream,
    context: ei::Context,
    event_tx: Sender<(CaptureHandle, Event)>,
    release_session: Arc<Notify>,
    current_client: Rc<Cell<Option<CaptureHandle>>>,
) -> Result<(), CaptureError> {
    loop {
        let ei_event = ei_event_stream
            .next()
            .await
            .ok_or(CaptureError::EndOfStream)?
            .map_err(ReisConvertEventStreamError::from)?;
        log::trace!("from ei: {ei_event:?}");
        let client = current_client.get();
        handle_ei_event(ei_event, client, &context, &event_tx, &release_session).await?;
    }
}

impl<'a> LibeiInputCapture<'a> {
    pub async fn new() -> std::result::Result<Self, LibeiCaptureCreationError> {
        let input_capture = Box::pin(InputCapture::new().await?);
        let input_capture_ptr = input_capture.as_ref().get_ref() as *const InputCapture<'static>;
        let first_session = Some(create_session(unsafe { &*input_capture_ptr }).await?);

        let (event_tx, event_rx) = mpsc::channel(1);
        let (notify_capture, notify_rx) = mpsc::channel(1);
        let notify_release = Arc::new(Notify::new());

        let cancellation_token = CancellationToken::new();

        let capture = do_capture(
            input_capture_ptr,
            notify_rx,
            notify_release.clone(),
            first_session,
            event_tx,
            cancellation_token.clone(),
        );
        let capture_task = tokio::task::spawn_local(capture);

        let producer = Self {
            input_capture,
            event_rx,
            capture_task,
            notify_capture,
            notify_release,
            cancellation_token,
        };

        Ok(producer)
    }
}

async fn do_capture<'a>(
    input_capture: *const InputCapture<'a>,
    mut capture_event: Receiver<LibeiNotifyEvent>,
    notify_release: Arc<Notify>,
    session: Option<(Session<'a>, BitFlags<Capabilities>)>,
    event_tx: Sender<(CaptureHandle, Event)>,
    cancellation_token: CancellationToken,
) -> Result<(), CaptureError> {
    let mut session = session.map(|s| s.0);

    /* safety: libei_task does not outlive Self */
    let input_capture = unsafe { &*input_capture };
    let mut active_clients: Vec<(CaptureHandle, Position)> = vec![];
    let mut next_barrier_id = 1u32;

    let mut zones_changed = input_capture.receive_zones_changed().await?;

    loop {
        // do capture session
        let cancel_session = CancellationToken::new();
        let cancel_update = CancellationToken::new();

        let mut capture_event_occured: Option<LibeiNotifyEvent> = None;
        let mut zones_have_changed = false;

        // kill session if clients need to be updated
        let handle_session_update_request = async {
            tokio::select! {
                _ = cancellation_token.cancelled() => {
                    log::debug!("cancelled")
                }, /* exit requested */
                _ = cancel_update.cancelled() => {
                    log::debug!("update task cancelled");
                }, /* session exited */
                _ = zones_changed.next() => {
                    log::debug!("zones changed!");
                    zones_have_changed = true
                }, /* zones have changed */
                e = capture_event.recv() => if let Some(e) = e { /* clients changed */
                    log::debug!("capture event: {e:?}");
                    capture_event_occured.replace(e);
                },
            }
            // kill session (might already be dead!)
            log::debug!("=> cancelling session");
            cancel_session.cancel();
        };

        if !active_clients.is_empty() {
            // create session
            let mut session = match session.take() {
                Some(s) => s,
                None => create_session(input_capture).await?.0,
            };

            let capture_session = do_capture_session(
                input_capture,
                &mut session,
                &event_tx,
                &mut active_clients,
                &mut next_barrier_id,
                &notify_release,
                cancel_session.clone(),
                cancel_update.clone(),
            );

            let (capture_result, ()) = tokio::join!(capture_session, handle_session_update_request);
            log::debug!("capture session + session_update task done!");

            // disable capture
            log::debug!("disabling input capture");
            if let Err(e) = input_capture.disable(&session).await {
                log::warn!("input_capture.disable(&session) {e}");
            }
            if let Err(e) = session.close().await {
                log::warn!("session.close(): {e}");
            }

            // propagate error from capture session
            capture_result?;
        } else {
            handle_session_update_request.await;
        }

        // update clients if requested
        if let Some(event) = capture_event_occured.take() {
            match event {
                LibeiNotifyEvent::Create(c, p) => active_clients.push((c, p)),
                LibeiNotifyEvent::Destroy(c) => active_clients.retain(|(h, _)| *h != c),
            }
        }

        // break
        if cancellation_token.is_cancelled() {
            break Ok(());
        }
    }
}

async fn do_capture_session(
    input_capture: &InputCapture<'_>,
    session: &mut Session<'_>,
    event_tx: &Sender<(CaptureHandle, Event)>,
    active_clients: &mut Vec<(CaptureHandle, Position)>,
    next_barrier_id: &mut u32,
    notify_release: &Notify,
    cancel_session: CancellationToken,
    cancel_update: CancellationToken,
) -> Result<(), CaptureError> {
    // current client
    let current_client = Rc::new(Cell::new(None));

    // connect to eis server
    let (context, ei_event_stream) = connect_to_eis(input_capture, session).await?;

    // set barriers
    let client_for_barrier_id =
        update_barriers(input_capture, session, active_clients, next_barrier_id).await?;

    log::debug!("enabling session");
    input_capture.enable(session).await?;

    // cancellation token to release session
    let release_session = Arc::new(Notify::new());

    // async event task
    let cancel_ei_handler = CancellationToken::new();
    let event_chan = event_tx.clone();
    let client = current_client.clone();
    let cancel_session_clone = cancel_session.clone();
    let release_session_clone = release_session.clone();
    let cancel_ei_handler_clone = cancel_ei_handler.clone();
    let ei_task = async move {
        tokio::select! {
            r = libei_event_handler(
                ei_event_stream,
                context,
                event_chan,
                release_session_clone,
                client,
            ) => {
                log::debug!("libei exited: {r:?} cancelling session task");
                cancel_session_clone.cancel();
            }
            _ = cancel_ei_handler_clone.cancelled() => {},
        }
        Ok::<(), CaptureError>(())
    };

    let capture_session_task = async {
        // receiver for activation tokens
        let mut activated = input_capture.receive_activated().await?;
        let mut ei_devices_changed = false;
        loop {
            tokio::select! {
                activated = activated.next() => {
                    let activated = activated.ok_or(CaptureError::ActivationClosed)?;
                    log::debug!("activated: {activated:?}");

                    let client = *client_for_barrier_id
                        .get(&activated.barrier_id())
                        .expect("invalid barrier id");
                    current_client.replace(Some(client));

                    // client entered => send event
                    event_tx.send((client, Event::Enter())).await.expect("no channel");

                    tokio::select! {
                        _ = notify_release.notified() => { /* capture release */
                            log::debug!("release session requested");
                        },
                        _ = release_session.notified() => { /* release session */
                            log::debug!("ei devices changed");
                            ei_devices_changed = true;
                        },
                        _ = cancel_session.cancelled() => { /* kill session notify */
                            log::debug!("session cancel requested");
                            break
                        },
                    }

                    release_capture(input_capture, session, activated, client, active_clients).await?;

                }
                _ = notify_release.notified() => { /* capture release -> we are not capturing anyway, so ignore */
                    log::debug!("release session requested");
                },
                _ = release_session.notified() => { /* release session */
                    log::debug!("ei devices changed");
                    ei_devices_changed = true;
                },
                _ = cancel_session.cancelled() => { /* kill session notify */
                    log::debug!("session cancel requested");
                    break
                },
            }
            if ei_devices_changed {
                /* for whatever reason, GNOME seems to kill the session
                 * as soon as devices are added or removed, so we need
                 * to cancel */
                break;
            }
        }
        // cancel libei task
        log::debug!("session exited: killing libei task");
        cancel_ei_handler.cancel();
        Ok::<(), CaptureError>(())
    };

    let (a, b) = tokio::join!(ei_task, capture_session_task);

    cancel_update.cancel();

    log::debug!("both session and ei task finished!");
    a?;
    b?;

    Ok(())
}

async fn release_capture(
    input_capture: &InputCapture<'_>,
    session: &Session<'_>,
    activated: Activated,
    current_client: CaptureHandle,
    active_clients: &[(CaptureHandle, Position)],
) -> Result<(), CaptureError> {
    log::debug!("releasing input capture {}", activated.activation_id());
    let (x, y) = activated.cursor_position();
    let pos = active_clients
        .iter()
        .filter(|(c, _)| *c == current_client)
        .map(|(_, p)| p)
        .next()
        .unwrap(); // FIXME
    let (dx, dy) = match pos {
        // offset cursor position to not enter again immediately
        Position::Left => (1., 0.),
        Position::Right => (-1., 0.),
        Position::Top => (0., 1.),
        Position::Bottom => (0., -1.),
    };
    // release 1px to the right of the entered zone
    let cursor_position = (x as f64 + dx, y as f64 + dy);
    input_capture
        .release(session, activated.activation_id(), cursor_position)
        .await?;
    Ok(())
}

async fn handle_ei_event(
    ei_event: EiEvent,
    current_client: Option<CaptureHandle>,
    context: &ei::Context,
    event_tx: &Sender<(CaptureHandle, Event)>,
    release_session: &Notify,
) -> Result<(), CaptureError> {
    match ei_event {
        EiEvent::SeatAdded(s) => {
            s.seat.bind_capabilities(&[
                DeviceCapability::Pointer,
                DeviceCapability::PointerAbsolute,
                DeviceCapability::Keyboard,
                DeviceCapability::Touch,
                DeviceCapability::Scroll,
                DeviceCapability::Button,
            ]);
            context.flush().map_err(|e| io::Error::new(e.kind(), e))?;
        }
        EiEvent::SeatRemoved(_) | /* EiEvent::DeviceAdded(_) | */ EiEvent::DeviceRemoved(_) => {
            log::debug!("releasing session: {ei_event:?}");
            release_session.notify_waiters();
        }
        EiEvent::DevicePaused(_) | EiEvent::DeviceResumed(_) => {}
        EiEvent::DeviceStartEmulating(_) => log::debug!("START EMULATING"),
        EiEvent::DeviceStopEmulating(_) => log::debug!("STOP EMULATING"),
        EiEvent::Disconnected(d) => {
            return Err(CaptureError::Disconnected(format!("{:?}", d.reason)))
        }
        _ => {
            if let Some(handle) = current_client {
                for event in to_input_events(ei_event).into_iter() {
                    event_tx.send((handle, event)).await.expect("no channel");
                }
            }
        }
    }
    Ok(())
}

/* not pretty but saves a heap allocation */
enum Events {
    None,
    One(Event),
    Two(Event, Event),
}

impl Events {
    fn into_iter(self) -> impl Iterator<Item = Event> {
        EventIterator::new(self)
    }
}

struct EventIterator {
    events: [Option<Event>; 2],
    pos: usize,
}

impl EventIterator {
    fn new(events: Events) -> Self {
        let events = match events {
            Events::None => [None, None],
            Events::One(e) => [Some(e), None],
            Events::Two(e, f) => [Some(e), Some(f)],
        };
        Self { events, pos: 0 }
    }
}

impl Iterator for EventIterator {
    type Item = Event;

    fn next(&mut self) -> Option<Self::Item> {
        let res = if self.pos >= self.events.len() {
            None
        } else {
            self.events[self.pos]
        };
        self.pos += 1;
        res
    }
}

fn to_input_events(ei_event: EiEvent) -> Events {
    match ei_event {
        EiEvent::KeyboardModifiers(mods) => {
            let modifier_event = KeyboardEvent::Modifiers {
                mods_depressed: mods.depressed,
                mods_latched: mods.latched,
                mods_locked: mods.locked,
                group: mods.group,
            };
            Events::One(Event::Keyboard(modifier_event))
        }
        EiEvent::Frame(_) => Events::None, /* FIXME */
        EiEvent::PointerMotion(motion) => {
            let motion_event = PointerEvent::Motion {
                time: motion.time as u32,
                dx: motion.dx as f64,
                dy: motion.dy as f64,
            };
            Events::One(Event::Pointer(motion_event))
        }
        EiEvent::PointerMotionAbsolute(_) => Events::None,
        EiEvent::Button(button) => {
            let button_event = PointerEvent::Button {
                time: button.time as u32,
                button: button.button,
                state: match button.state {
                    ButtonState::Released => 0,
                    ButtonState::Press => 1,
                },
            };
            Events::One(Event::Pointer(button_event))
        }
        EiEvent::ScrollDelta(delta) => {
            let dy = Event::Pointer(PointerEvent::Axis {
                time: 0,
                axis: 0,
                value: delta.dy as f64,
            });
            let dx = Event::Pointer(PointerEvent::Axis {
                time: 0,
                axis: 1,
                value: delta.dx as f64,
            });
            if delta.dy != 0. && delta.dx != 0. {
                Events::Two(dy, dx)
            } else if delta.dy != 0. {
                Events::One(dy)
            } else if delta.dx != 0. {
                Events::One(dx)
            } else {
                Events::None
            }
        }
        EiEvent::ScrollStop(_) => Events::None,   /* TODO */
        EiEvent::ScrollCancel(_) => Events::None, /* TODO */
        EiEvent::ScrollDiscrete(scroll) => {
            let dy = Event::Pointer(PointerEvent::AxisDiscrete120 {
                axis: 0,
                value: scroll.discrete_dy,
            });
            let dx = Event::Pointer(PointerEvent::AxisDiscrete120 {
                axis: 1,
                value: scroll.discrete_dx,
            });
            if scroll.discrete_dy != 0 && scroll.discrete_dx != 0 {
                Events::Two(dy, dx)
            } else if scroll.discrete_dy != 0 {
                Events::One(dy)
            } else if scroll.discrete_dx != 0 {
                Events::One(dx)
            } else {
                Events::None
            }
        }
        EiEvent::KeyboardKey(key) => {
            let key_event = KeyboardEvent::Key {
                key: key.key,
                state: match key.state {
                    KeyState::Press => 1,
                    KeyState::Released => 0,
                },
                time: key.time as u32,
            };
            Events::One(Event::Keyboard(key_event))
        }
        EiEvent::TouchDown(_) => Events::None,   /* TODO */
        EiEvent::TouchUp(_) => Events::None,     /* TODO */
        EiEvent::TouchMotion(_) => Events::None, /* TODO */
        _ => Events::None,
    }
}

#[async_trait]
impl<'a> LanMouseInputCapture for LibeiInputCapture<'a> {
    async fn create(&mut self, handle: CaptureHandle, pos: Position) -> Result<(), CaptureError> {
        let _ = self
            .notify_capture
            .send(LibeiNotifyEvent::Create(handle, pos))
            .await;
        Ok(())
    }

    async fn destroy(&mut self, handle: CaptureHandle) -> Result<(), CaptureError> {
        let _ = self
            .notify_capture
            .send(LibeiNotifyEvent::Destroy(handle))
            .await;
        Ok(())
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        self.notify_release.notify_one();
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        self.cancellation_token.cancel();
        let task = &mut self.capture_task;
        log::debug!("waiting for capture to terminate...");
        let res = task.await.expect("libei task panic");
        log::debug!("done!");
        res
    }
}

impl<'a> Stream for LibeiInputCapture<'a> {
    type Item = Result<(CaptureHandle, Event), CaptureError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.capture_task.poll_unpin(cx) {
            Poll::Ready(r) => match r.expect("failed to join") {
                Ok(()) => Poll::Ready(None),
                Err(e) => Poll::Ready(Some(Err(e))),
            },
            Poll::Pending => self.event_rx.poll_recv(cx).map(|e| e.map(Result::Ok)),
        }
    }
}
