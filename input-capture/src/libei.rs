use ashpd::{
    desktop::{
        Session,
        input_capture::{
            Activated, ActivatedBarrier, Barrier, BarrierID, Capabilities, CreateSessionOptions,
            InputCapture, Region, ReleaseOptions, Zones,
        },
    },
    enumflags2::BitFlags,
};
use async_trait::async_trait;
use futures::{Future, FutureExt, StreamExt};
use reis::{
    ei::{self, handshake::ContextType},
    event::{Connection, DeviceCapability, EiEvent},
    tokio::EiConvertEventStream,
};
use std::{
    cell::Cell,
    collections::HashMap,
    env, io,
    num::NonZeroU32,
    os::unix::net::UnixStream,
    pin::Pin,
    rc::Rc,
    sync::{Arc, LazyLock},
    task::{Context, Poll},
};
use tokio::{
    sync::{
        Notify,
        mpsc::{self, Receiver, Sender},
        watch,
    },
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use futures_core::Stream;

use input_event::Event;

use crate::CaptureEvent;

use super::{
    Capture as LanMouseInputCapture, Position,
    error::{CaptureError, LibeiCaptureCreationError},
};

/* there is a bug in xdg-remote-desktop-portal-gnome / mutter that
 * prevents receiving further events after a session has been disabled once.
 * Therefore the session needs to be recreated when the barriers are updated */

/* mutter also kills the session whenever ei devices come and go, so there the
 * whole session has to be torn down and recreated on every device change.
 * Elsewhere that is pure overhead: each restart costs a CreateSession +
 * ConnectToEIS round trip, and compositors that keep per-session state around
 * (hyprland leaks a keymap fd per eis session, see hyprwm/Hyprland) can be
 * driven out of file descriptors by the churn.
 * Set LM_RESTART_SESSION_ON_DEVICE_CHANGE=1/0 to override the default. */
static RESTART_SESSION_ON_DEVICE_CHANGE: LazyLock<bool> =
    LazyLock::new(restart_session_on_device_change);

fn restart_session_on_device_change() -> bool {
    match env::var("LM_RESTART_SESSION_ON_DEVICE_CHANGE").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|desktops| desktops.to_uppercase().split(':').any(|d| d == "GNOME")),
    }
}

/// events that necessitate restarting the capture session
#[derive(Clone, Copy, Debug)]
enum LibeiNotifyEvent {
    Create(Position),
    Destroy(Position),
}

#[allow(dead_code)]
pub struct LibeiInputCapture {
    input_capture: Pin<Box<InputCapture>>,
    capture_task: JoinHandle<Result<(), CaptureError>>,
    event_rx: Receiver<(Position, CaptureEvent)>,
    availability: watch::Receiver<bool>,
    notify_capture: Sender<LibeiNotifyEvent>,
    notify_release: Arc<Notify>,
    cancellation_token: CancellationToken,
    terminated: bool,
}

/// returns (start pos, end pos), inclusive
fn pos_to_barrier(r: &Region, pos: Position) -> (i32, i32, i32, i32) {
    let (x, y) = (r.x_offset(), r.y_offset());
    let (w, h) = (r.width() as i32, r.height() as i32);
    match pos {
        Position::Left => (x, y, x, y + h - 1),
        Position::Right => (x + w, y, x + w, y + h - 1),
        Position::Top => (x, y, x + w - 1, y),
        Position::Bottom => (x, y + h, x + w - 1, y + h),
    }
}

/// Ashpd does not expose fields
#[derive(Clone, Copy, Debug)]
struct ICBarrier {
    barrier_id: BarrierID,
    position: (i32, i32, i32, i32),
}

impl ICBarrier {
    fn new(barrier_id: BarrierID, position: (i32, i32, i32, i32)) -> Self {
        Self {
            barrier_id,
            position,
        }
    }
}

impl From<ICBarrier> for Barrier {
    fn from(barrier: ICBarrier) -> Self {
        Barrier::new(barrier.barrier_id, barrier.position)
    }
}

fn select_barriers(
    zones: &Zones,
    clients: &[Position],
    next_barrier_id: &mut NonZeroU32,
) -> (Vec<ICBarrier>, HashMap<BarrierID, Position>) {
    let mut pos_for_barrier = HashMap::new();
    let mut barriers: Vec<ICBarrier> = vec![];

    for pos in clients {
        let mut client_barriers = zones
            .regions()
            .iter()
            .map(|r| {
                let id = *next_barrier_id;
                *next_barrier_id = next_barrier_id
                    .checked_add(1)
                    .expect("barrier id out of range");
                let position = pos_to_barrier(r, *pos);
                pos_for_barrier.insert(id, *pos);
                ICBarrier::new(id, position)
            })
            .collect();
        barriers.append(&mut client_barriers);
    }
    (barriers, pos_for_barrier)
}

async fn update_barriers(
    input_capture: &InputCapture,
    session: &Session<InputCapture>,
    active_clients: &[Position],
    next_barrier_id: &mut NonZeroU32,
) -> Result<(Vec<ICBarrier>, HashMap<BarrierID, Position>), ashpd::Error> {
    let zones = input_capture
        .zones(session, Default::default())
        .await?
        .response()?;
    log::debug!("zones: {zones:?}");

    let (barriers, id_map) = select_barriers(&zones, active_clients, next_barrier_id);
    log::debug!("barriers: {barriers:?}");
    log::debug!("client for barrier id: {id_map:?}");

    let ashpd_barriers: Vec<Barrier> = barriers.iter().copied().map(|b| b.into()).collect();
    let response = input_capture
        .set_pointer_barriers(
            session,
            &ashpd_barriers,
            zones.zone_set(),
            Default::default(),
        )
        .await?;
    let response = response.response()?;
    log::debug!("{response:?}");
    Ok((barriers, id_map))
}

async fn create_session(
    input_capture: &InputCapture,
) -> std::result::Result<(Session<InputCapture>, BitFlags<Capabilities>), ashpd::Error> {
    log::debug!("creating input capture session");
    let create_session_options = CreateSessionOptions::default().set_capabilities(
        Capabilities::Keyboard | Capabilities::Pointer | Capabilities::Touchscreen,
    );
    input_capture
        .create_session(None, create_session_options)
        .await
}

async fn connect_to_eis(
    input_capture: &InputCapture,
    session: &Session<InputCapture>,
) -> Result<(ei::Context, Connection, EiConvertEventStream), CaptureError> {
    log::debug!("connect_to_eis");
    let fd = input_capture
        .connect_to_eis(session, Default::default())
        .await?;

    // create unix stream from fd
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(true)?;

    // create ei context
    let context = ei::Context::new(stream)?;
    let (conn, event_stream) = context
        .handshake_tokio("de.feschber.LanMouse", ContextType::Receiver)
        .await?;

    Ok((context, conn, event_stream))
}

fn libei_stream_error(error: reis::Error) -> CaptureError {
    match error {
        reis::Error::Io(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::NotConnected
                    | io::ErrorKind::UnexpectedEof
            ) =>
        {
            log::debug!("libei transport closed: {error}");
            CaptureError::EndOfStream
        }
        error => error.into(),
    }
}

async fn libei_event_handler(
    mut ei_event_stream: EiConvertEventStream,
    context: ei::Context,
    event_tx: Sender<(Position, CaptureEvent)>,
    release_session: Arc<Notify>,
    current_pos: Rc<Cell<Option<Position>>>,
) -> Result<(), CaptureError> {
    loop {
        let ei_event = ei_event_stream
            .next()
            .await
            .ok_or(CaptureError::EndOfStream)?
            .map_err(libei_stream_error)?;
        log::trace!("from ei: {ei_event:?}");
        let client = current_pos.get();
        match handle_ei_event(ei_event, client, &context, &event_tx, &release_session).await {
            Err(CaptureError::Disconnected(reason)) => {
                log::debug!("libei disconnected: {reason}");
                return Err(CaptureError::EndOfStream);
            }
            result => result?,
        }
    }
}

impl LibeiInputCapture {
    pub async fn new() -> std::result::Result<Self, LibeiCaptureCreationError> {
        // A dedicated connection lets dropping a cancelled authorization close
        // its portal requests without disturbing RemoteDesktop emulation.
        let connection = ashpd::zbus::Connection::session()
            .await
            .map_err(ashpd::Error::from)?;
        let input_capture = Box::pin(InputCapture::with_connection(connection).await?);
        let input_capture_ptr = input_capture.as_ref().get_ref() as *const InputCapture;
        let first_session = Some(create_session(unsafe { &*input_capture_ptr }).await?);
        let (availability_tx, availability) = watch::channel(false);

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
            availability_tx,
        );
        let capture_task = tokio::task::spawn_local(capture);

        let producer = Self {
            input_capture,
            event_rx,
            capture_task,
            availability,
            notify_capture,
            notify_release,
            cancellation_token,
            terminated: false,
        };

        Ok(producer)
    }
}

async fn await_session<T>(
    create: impl Future<Output = Result<T, ashpd::Error>>,
    updates: &mut Receiver<LibeiNotifyEvent>,
    clients: &mut Vec<Position>,
    zones_changed: &mut (impl Stream<Item = ()> + Unpin),
    cancel: &CancellationToken,
) -> Result<Option<T>, CaptureError> {
    tokio::pin!(create);
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(None),
            update = updates.recv() => match update {
                Some(update) => update_clients(clients, update),
                None => return Ok(None),
            },
            zone = zones_changed.next() => {
                if zone.is_none() {
                    return Err(CaptureError::EndOfStream);
                }
                // A new session will query the current zones before enabling.
            },
            result = &mut create => return Ok(Some(result?)),
        }
    }
}

fn update_clients(clients: &mut Vec<Position>, event: LibeiNotifyEvent) {
    match event {
        LibeiNotifyEvent::Create(pos) => clients.push(pos),
        LibeiNotifyEvent::Destroy(pos) => clients.retain(|&client| client != pos),
    }
}

async fn capture_until_closed(
    capture: impl Future<Output = Result<(), CaptureError>>,
    closed: impl Future,
    cancel_update: &CancellationToken,
    cancel_session: &CancellationToken,
) -> (Result<(), CaptureError>, bool) {
    let result = tokio::select! {
        // A closed session takes precedence over a concurrent InvalidSession
        // response, so the expired session is not released or reused.
        biased;
        _ = closed => {
            log::info!("input capture portal session closed; recreating session");
            (Err(CaptureError::ActivationClosed), true)
        },
        result = capture => (result, false),
        _ = cancel_session.cancelled() => (Ok(()), false),
    };
    // Also wake the barrier-update task when cancellation drops capture's
    // future before its usual cleanup path can run.
    cancel_update.cancel();
    result
}

async fn do_capture(
    input_capture: *const InputCapture,
    mut capture_event: Receiver<LibeiNotifyEvent>,
    notify_release: Arc<Notify>,
    session: Option<(Session<InputCapture>, BitFlags<Capabilities>)>,
    event_tx: Sender<(Position, CaptureEvent)>,
    cancellation_token: CancellationToken,
    availability: watch::Sender<bool>,
) -> Result<(), CaptureError> {
    let mut session = session.map(|s| s.0);

    /* safety: libei_task does not outlive Self */
    let input_capture = unsafe { &*input_capture };
    let mut active_clients: Vec<Position> = vec![];
    let mut next_barrier_id = NonZeroU32::new(1).expect("id must be non-zero");

    let mut zones_changed = input_capture.receive_zones_changed().await?.map(|_| ());

    loop {
        if !active_clients.is_empty() && session.is_none() {
            availability.send_replace(false);
            session = match await_session(
                create_session(input_capture),
                &mut capture_event,
                &mut active_clients,
                &mut zones_changed,
                &cancellation_token,
            )
            .await?
            {
                Some((session, _)) => Some(session),
                None => return Ok(()),
            };
        }
        // do capture session
        let cancel_session = CancellationToken::new();
        let cancel_update = CancellationToken::new();

        let mut capture_event_occured: Option<LibeiNotifyEvent> = None;
        // A zone change cancels the current session; the next one queries zones.

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
                },
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
            availability.send_replace(false);
            let session = match session.take() {
                Some(session) => session,
                None => return Err(CaptureError::EndOfStream),
            };

            let mut closed = session.receive_closed().await?;
            let capture_session = capture_until_closed(
                do_capture_session(
                    input_capture,
                    &session,
                    &event_tx,
                    &active_clients,
                    &mut next_barrier_id,
                    &notify_release,
                    CaptureSessionControl {
                        cancel_session: &cancel_session,
                        cancel_update: &cancel_update,
                        availability: &availability,
                    },
                ),
                closed.next(),
                &cancel_update,
                &cancel_session,
            );

            let ((capture_result, closed_by_portal), ()) =
                tokio::join!(capture_session, handle_session_update_request);
            log::debug!("capture session + session_update task done!");
            availability.send_replace(false);

            if !closed_by_portal {
                log::debug!("disabling input capture");
                if let Err(e) = input_capture.disable(&session, Default::default()).await {
                    log::warn!("input_capture.disable(&session) {e}");
                }
                if let Err(e) = session.close().await {
                    log::warn!("session.close(): {e}");
                }
            }

            // propagate error from capture session
            capture_result?;
        } else {
            availability.send_replace(true);
            handle_session_update_request.await;
        }

        // update clients if requested
        if let Some(event) = capture_event_occured.take() {
            update_clients(&mut active_clients, event);
        }

        // break
        if cancellation_token.is_cancelled() {
            break Ok(());
        }
    }
}

struct CaptureSessionControl<'a> {
    cancel_session: &'a CancellationToken,
    cancel_update: &'a CancellationToken,
    availability: &'a watch::Sender<bool>,
}

async fn do_capture_session(
    input_capture: &InputCapture,
    session: &Session<InputCapture>,
    event_tx: &Sender<(Position, CaptureEvent)>,
    active_clients: &[Position],
    next_barrier_id: &mut NonZeroU32,
    notify_release: &Notify,
    control: CaptureSessionControl<'_>,
) -> Result<(), CaptureError> {
    let CaptureSessionControl {
        cancel_session,
        cancel_update,
        availability,
    } = control;
    // current client
    let current_pos = Rc::new(Cell::new(None));

    // connect to eis server
    let (context, _conn, ei_event_stream) = connect_to_eis(input_capture, session).await?;

    // set barriers
    let (barriers, pos_for_barrier_id) =
        update_barriers(input_capture, session, active_clients, next_barrier_id).await?;

    log::debug!("enabling session");
    input_capture.enable(session, Default::default()).await?;
    availability.send_replace(true);

    // cancellation token to release session
    let release_session = Arc::new(Notify::new());

    let capture_session_task = async {
        // receiver for activation tokens
        let mut activated = input_capture.receive_activated().await?;
        let mut ei_devices_changed = false;
        loop {
            tokio::select! {
                activated = activated.next() => {
                    let activated = activated.ok_or(CaptureError::ActivationClosed)?;
                    log::debug!("activated: {activated:?}");

                    // get barrier id from activation
                    let barrier_id = match activated.barrier_id() {
                        Some(ActivatedBarrier::Barrier(id)) => id,
                        // workaround for KDE plasma not reporting barrier ids
                        Some(ActivatedBarrier::UnknownBarrier) | None => find_corresponding_client(&barriers, activated.cursor_position().expect("no cursor position reported by compositor")),
                    };

                    // find client corresponding to barrier
                    let pos = match pos_for_barrier_id.get(&barrier_id) {
                        Some(id) => *id,
                        None => {
                            log::warn!("INVALID BARRIER ID: Id {barrier_id} does not exist!");
                            let id = find_corresponding_client(&barriers, activated.cursor_position().expect("no cursor position reported by compositor"));
                            let pos = *pos_for_barrier_id.get(&id).expect("invalid barrier id");
                            pos
                        },
                    };
                    current_pos.replace(Some(pos));

                    // client entered => send event
                    event_tx.send((pos, CaptureEvent::Begin)).await.expect("no channel");

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

                    release_capture(input_capture, session, activated, pos).await?;

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
        log::debug!("capture session finished");
        Ok::<(), CaptureError>(())
    };

    drive_capture_session(
        libei_event_handler(
            ei_event_stream,
            context,
            event_tx.clone(),
            release_session.clone(),
            current_pos.clone(),
        ),
        capture_session_task,
        cancel_session,
        cancel_update,
    )
    .await
}

async fn drive_capture_session(
    ei: impl Future<Output = Result<(), CaptureError>>,
    capture: impl Future<Output = Result<(), CaptureError>>,
    cancel_session: &CancellationToken,
    cancel_update: &CancellationToken,
) -> Result<(), CaptureError> {
    let cancel_ei = CancellationToken::new();
    let ei_task = async {
        tokio::select! {
            biased;
            result = ei => {
                cancel_session.cancel();
                result
            },
            _ = cancel_ei.cancelled() => Ok(()),
        }
    };
    let capture_task = async {
        let result = capture.await;
        cancel_ei.cancel();
        result
    };

    let (a, b) = tokio::join!(ei_task, capture_task);

    cancel_update.cancel();

    log::debug!("both session and ei task finished!");
    a?;
    b?;

    Ok(())
}

async fn release_capture(
    input_capture: &InputCapture,
    session: &Session<InputCapture>,
    activated: Activated,
    current_pos: Position,
) -> Result<(), CaptureError> {
    if let Some(activation_id) = activated.activation_id() {
        log::debug!("releasing input capture {activation_id}");
    }
    let (x, y) = activated
        .cursor_position()
        .expect("compositor did not report cursor position!");
    log::debug!("client entered @ ({x}, {y})");
    let (dx, dy) = match current_pos {
        // offset cursor position to not enter again immediately
        Position::Left => (1., 0.),
        Position::Right => (-1., 0.),
        Position::Top => (0., 1.),
        Position::Bottom => (0., -1.),
    };
    // release 1px to the right of the entered zone
    let cursor_position = (x as f64 + dx, y as f64 + dy);
    let release_options = ReleaseOptions::default()
        .set_activation_id(activated.activation_id())
        .set_cursor_position(Some(cursor_position));
    input_capture.release(session, release_options).await?;
    Ok(())
}

fn find_corresponding_client(barriers: &[ICBarrier], pos: (f32, f32)) -> BarrierID {
    barriers
        .iter()
        .copied()
        .min_by_key(|b| {
            let (x1, y1, x2, y2) = b.position;
            let (x1, y1, x2, y2) = (x1 as f32, y1 as f32, x2 as f32, y2 as f32);
            distance_to_line(((x1, y1), (x2, y2)), pos) as i32
        })
        .expect("could not find barrier corresponding to client")
        .barrier_id
}

fn distance_to_line(line: ((f32, f32), (f32, f32)), p: (f32, f32)) -> f32 {
    let ((x1, y1), (x2, y2)) = line;
    let (x0, y0) = p;
    /*
     * we use the fact that for the triangle spanned by the line and p,
     * the height of the triangle is the desired distance and can be calculated by
     * h = 2A / b with b being the line_length and
     */
    let double_triangle_area = ((y2 - y1) * x0 - (x2 - x1) * y0 + x2 * y1 - y2 * x1).abs();
    let line_length = ((y2 - y1).powf(2.0) + (x2 - x1).powf(2.0)).sqrt();
    let distance = double_triangle_area / line_length;
    log::debug!("distance to line({line:?}, {p:?}) = {distance}");
    distance
}

async fn handle_ei_event(
    ei_event: EiEvent,
    current_client: Option<Position>,
    context: &ei::Context,
    event_tx: &Sender<(Position, CaptureEvent)>,
    release_session: &Notify,
) -> Result<(), CaptureError> {
    let all_capabilities = DeviceCapability::Pointer
        | DeviceCapability::PointerAbsolute
        | DeviceCapability::Keyboard
        | DeviceCapability::Touch
        | DeviceCapability::Scroll
        | DeviceCapability::Button;
    match ei_event {
        EiEvent::SeatAdded(s) => {
            s.seat.bind_capabilities(all_capabilities);
            context.flush().map_err(|e| io::Error::new(e.kind(), e))?;
        }
        EiEvent::SeatRemoved(_) => {
            log::debug!("releasing session: {ei_event:?}");
            release_session.notify_waiters();
        }
        /* EiEvent::DeviceAdded(_) | */
        EiEvent::DeviceRemoved(_) => {
            if *RESTART_SESSION_ON_DEVICE_CHANGE {
                log::debug!("releasing session: {ei_event:?}");
                release_session.notify_waiters();
            } else {
                log::debug!("ignoring device change: {ei_event:?}");
            }
        }
        EiEvent::DevicePaused(_) | EiEvent::DeviceResumed(_) => {}
        EiEvent::DeviceStartEmulating(_) => log::debug!("START EMULATING"),
        EiEvent::DeviceStopEmulating(_) => log::debug!("STOP EMULATING"),
        EiEvent::Disconnected(d) => {
            return Err(CaptureError::Disconnected(format!("{:?}", d.reason)));
        }
        _ => {
            if let Some(pos) = current_client {
                for event in Event::from_ei_event(ei_event) {
                    event_tx
                        .send((pos, CaptureEvent::Input(event)))
                        .await
                        .expect("no channel");
                }
            }
        }
    }
    Ok(())
}

#[async_trait]
impl LanMouseInputCapture for LibeiInputCapture {
    fn availability(&self) -> Option<watch::Receiver<bool>> {
        Some(self.availability.clone())
    }

    async fn create(&mut self, pos: Position) -> Result<(), CaptureError> {
        let _ = self
            .notify_capture
            .send(LibeiNotifyEvent::Create(pos))
            .await;
        Ok(())
    }

    async fn destroy(&mut self, pos: Position) -> Result<(), CaptureError> {
        let _ = self
            .notify_capture
            .send(LibeiNotifyEvent::Destroy(pos))
            .await;
        Ok(())
    }

    async fn release(&mut self) -> Result<(), CaptureError> {
        self.notify_release.notify_waiters();
        Ok(())
    }

    async fn terminate(&mut self) -> Result<(), CaptureError> {
        self.cancellation_token.cancel();
        let task = &mut self.capture_task;
        log::debug!("waiting for capture to terminate...");
        let res = if !task.is_finished() {
            task.await.expect("libei task panic")
        } else {
            Ok(())
        };
        self.terminated = true;
        log::debug!("done!");
        res
    }
}

impl Drop for LibeiInputCapture {
    fn drop(&mut self) {
        if !self.terminated {
            /* this workaround is needed until async drop is stabilized */
            panic!("LibeiInputCapture dropped without being terminated!");
        }
    }
}

impl Stream for LibeiInputCapture {
    type Item = Result<(Position, CaptureEvent), CaptureError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match self.capture_task.poll_unpin(cx) {
            Poll::Ready(r) => match r.expect("failed to join") {
                Ok(()) => Poll::Ready(None),
                Err(e) => Poll::Ready(Some(Err(e))),
            },
            Poll::Pending => self.event_rx.poll_recv(cx).map(|e| e.map(Result::Ok)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::{pending, ready},
        sync::atomic::{AtomicBool, Ordering},
    };

    struct CaptureResource(Arc<AtomicBool>);

    impl Drop for CaptureResource {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn closed_session_cancels_capture_and_unblocks_barrier_updates() {
        let dropped = Arc::new(AtomicBool::new(false));
        let resource = CaptureResource(dropped.clone());
        let capture = async move {
            let _resource = resource;
            pending::<Result<(), CaptureError>>().await
        };
        let cancel_update = CancellationToken::new();
        let (result, closed) = capture_until_closed(
            capture,
            ready(()),
            &cancel_update,
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(result, Err(CaptureError::ActivationClosed)) && closed);
        assert!(dropped.load(Ordering::SeqCst));
        assert!(cancel_update.is_cancelled());
    }

    #[tokio::test]
    async fn closure_wins_over_a_concurrent_stale_session_error() {
        let cancel_update = CancellationToken::new();
        let stale_session = CaptureError::Portal(ashpd::Error::NoResponse);
        let (result, closed) = capture_until_closed(
            ready(Err(stale_session)),
            ready(()),
            &cancel_update,
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(result, Err(CaptureError::ActivationClosed)) && closed);
    }

    #[tokio::test]
    async fn denied_authorization_is_propagated_without_recreating_the_session() {
        let cancel_update = CancellationToken::new();
        let denied = CaptureError::Portal(ashpd::Error::Response(
            ashpd::desktop::ResponseError::Cancelled,
        ));
        let (result, closed) = capture_until_closed(
            ready(Err(denied)),
            pending::<()>(),
            &cancel_update,
            &CancellationToken::new(),
        )
        .await;
        assert!(!closed);
        assert!(matches!(
            result,
            Err(CaptureError::Portal(ashpd::Error::Response(
                ashpd::desktop::ResponseError::Cancelled
            )))
        ));
    }

    #[tokio::test]
    async fn pending_authorization_is_cancelled_and_dropped() {
        let dropped = Arc::new(AtomicBool::new(false));
        let resource = CaptureResource(dropped.clone());
        let create = async move {
            let _resource = resource;
            pending::<Result<(), ashpd::Error>>().await
        };
        let (_tx, mut updates) = mpsc::channel(1);
        let mut clients = vec![Position::Right];
        let mut zones = futures::stream::pending();
        let cancel = CancellationToken::new();
        let wait = await_session(create, &mut updates, &mut clients, &mut zones, &cancel);
        tokio::pin!(wait);
        assert!(wait.as_mut().now_or_never().is_none());
        cancel.cancel();
        assert!(wait.await.unwrap().is_none());
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn pending_authorization_keeps_client_updates_without_restart() {
        let (grant, response) = tokio::sync::oneshot::channel();
        let (tx, mut updates) = mpsc::channel(2);
        let mut clients = vec![Position::Right];
        let mut zones = futures::stream::pending();
        let cancel = CancellationToken::new();
        {
            let wait = await_session(
                async { Ok(response.await.unwrap()) },
                &mut updates,
                &mut clients,
                &mut zones,
                &cancel,
            );
            tokio::pin!(wait);
            assert!(wait.as_mut().now_or_never().is_none());
            tx.send(LibeiNotifyEvent::Destroy(Position::Right))
                .await
                .unwrap();
            tx.send(LibeiNotifyEvent::Create(Position::Left))
                .await
                .unwrap();
            assert!(wait.as_mut().now_or_never().is_none());
            grant.send(42).unwrap();
            assert_eq!(wait.await.unwrap(), Some(42));
        }
        assert_eq!(clients, [Position::Left]);
    }

    #[tokio::test]
    async fn eis_failure_survives_session_cancellation() {
        let cancel = CancellationToken::new();
        let update = CancellationToken::new();
        let session = async {
            cancel.cancelled().await;
            Ok(())
        };
        let capture = drive_capture_session(
            ready(Err(CaptureError::EndOfStream)),
            session,
            &cancel,
            &update,
        );
        let (result, _) = capture_until_closed(capture, pending::<()>(), &update, &cancel).await;
        assert!(matches!(result, Err(CaptureError::EndOfStream)));
        assert!(update.is_cancelled());
    }

    #[tokio::test]
    async fn portal_setup_can_be_cancelled_before_eis_is_ready() {
        let cancel = CancellationToken::new();
        let update = CancellationToken::new();
        let capture = capture_until_closed(
            pending::<Result<(), CaptureError>>(),
            pending::<()>(),
            &update,
            &cancel,
        );
        tokio::pin!(capture);
        assert!(capture.as_mut().now_or_never().is_none());
        cancel.cancel();
        let (result, closed) = capture.await;
        assert!(result.is_ok() && !closed && update.is_cancelled());
    }

    #[tokio::test]
    async fn portal_error_drops_the_pending_eis_reader() {
        let dropped = Arc::new(AtomicBool::new(false));
        let resource = CaptureResource(dropped.clone());
        let ei = async move {
            let _resource = resource;
            pending::<Result<(), CaptureError>>().await
        };
        let result = drive_capture_session(
            ei,
            ready(Err(CaptureError::Portal(ashpd::Error::NoResponse))),
            &CancellationToken::new(),
            &CancellationToken::new(),
        )
        .await;
        assert!(matches!(
            result,
            Err(CaptureError::Portal(ashpd::Error::NoResponse))
        ));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn closed_transport_is_recoverable_but_permission_errors_are_not() {
        for kind in [
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::NotConnected,
            io::ErrorKind::UnexpectedEof,
        ] {
            assert!(matches!(
                libei_stream_error(reis::Error::Io(kind.into())),
                CaptureError::EndOfStream
            ));
        }
        assert!(matches!(
            libei_stream_error(reis::Error::Io(io::ErrorKind::PermissionDenied.into())),
            CaptureError::Reis(reis::Error::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied
        ));
    }
}
