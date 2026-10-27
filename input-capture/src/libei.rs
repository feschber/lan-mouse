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
use futures::{FutureExt, StreamExt};
use reis::{
    ei::{self, handshake::ContextType},
    event::{Connection, DeviceCapability, EiEvent},
    tokio::EiConvertEventStream,
};
use std::{
    cell::{Cell, RefCell},
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
        _ => is_gnome(),
    }
}

/* Alternative to the GNOME restart rule above: when the EI devices change
 * (mutter disables the session then -- e.g. on a monitor replug, such as a TV
 * switching to another input and back), re-apply the barriers and re-enable
 * the SAME session instead of creating a new one. A new session makes GNOME
 * ask the user for permission again, every time, because the portal has no
 * restore token for input capture.
 * On by default under GNOME; set LM_REENABLE_ON_DEVICE_CHANGE=1/0 to override. */
static REENABLE_ON_DEVICE_CHANGE: LazyLock<bool> = LazyLock::new(reenable_on_device_change);

fn reenable_on_device_change() -> bool {
    match env::var("LM_REENABLE_ON_DEVICE_CHANGE").as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        _ => is_gnome(),
    }
}

fn is_gnome() -> bool {
    env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|desktops| desktops.to_uppercase().split(':').any(|d| d == "GNOME"))
}

/// GNOME only: Mutter's DisplayConfig.MonitorsChanged, the one signal that
/// reliably fires on every monitor change (the portal's own Disabled /
/// ZonesChanged / EI device events were seen to fire only sometimes).
async fn mutter_monitors_changed() -> Option<Pin<Box<zbus::MessageStream>>> {
    if !*REENABLE_ON_DEVICE_CHANGE {
        return None;
    }
    let conn = zbus::Connection::session().await.ok()?;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.gnome.Mutter.DisplayConfig")
        .ok()?
        .member("MonitorsChanged")
        .ok()?
        .build();
    match zbus::MessageStream::for_match_rule(rule, &conn, Some(64)).await {
        Ok(stream) => {
            log::info!("watching Mutter MonitorsChanged");
            Some(Box::pin(stream))
        }
        Err(e) => {
            log::warn!("cannot watch Mutter MonitorsChanged: {e}");
            None
        }
    }
}

async fn next_monitors_changed(stream: &mut Option<Pin<Box<zbus::MessageStream>>>) {
    match stream {
        Some(s) => {
            if s.next().await.is_none() {
                std::future::pending::<()>().await
            }
        }
        None => std::future::pending::<()>().await,
    }
}

/// A TV switching inputs arrives as a burst of MonitorsChanged (2 pulses
/// ~0.6 s apart): wait until there has been none for 750 ms.
async fn settle_monitors(stream: &mut Option<Pin<Box<zbus::MessageStream>>>) {
    if let Some(s) = stream {
        while tokio::time::timeout(std::time::Duration::from_millis(750), s.next())
            .await
            .is_ok()
        {}
    }
}

/// Re-apply barriers on the current zones and re-enable `session` (see
/// REENABLE_ON_DEVICE_CHANGE). Waits briefly first so a monitor change that
/// arrives as several hot-plug pulses has settled.
async fn reapply_and_enable(
    input_capture: &InputCapture,
    session: &Session<InputCapture>,
    active_clients: &[Position],
    next_barrier_id: &mut NonZeroU32,
) -> Result<
    (
        Vec<ICBarrier>,
        HashMap<BarrierID, Position>,
        Vec<ZoneGeometry>,
    ),
    ashpd::Error,
> {
    tokio::time::sleep(std::time::Duration::from_millis(750)).await;
    let r = update_barriers(input_capture, session, active_clients, next_barrier_id).await?;
    if let Err(e) = input_capture.enable(session, Default::default()).await {
        // e.g. the session was never disabled: barriers are re-applied, keep going
        log::info!("enable after re-applying barriers: {e} (keeping the session)");
    }
    Ok(r)
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

/// Geometry of one zone: (width, height, x_offset, y_offset).
type ZoneGeometry = (u32, u32, i32, i32);

fn zone_geometry(zones: &Zones) -> Vec<ZoneGeometry> {
    zones
        .regions()
        .iter()
        .map(|r| (r.width(), r.height(), r.x_offset(), r.y_offset()))
        .collect()
}

/// The zones and barriers the running capture session was set up with.
type ActiveBarriers = RefCell<Option<(Vec<ZoneGeometry>, Vec<ICBarrier>)>>;

/// What ended the wait for a session update.
enum SessionUpdate {
    /// exit requested, the session ended by itself, or nothing to apply
    None,
    /// zones changed in a way the running session can't absorb
    ZonesChanged,
    /// clients were added / removed
    Clients(LibeiNotifyEvent),
}

/// Set `barriers`, and report the ids that still failed.
///
/// Mutter only adds barriers to a *disabled* session ("Session already
/// enabled"), while the portal clears the old barriers before adding the new
/// ones: setting barriers on an enabled session leaves it with none, and
/// nothing in the reply says why. This happens when a monitor returns: the
/// device-change path re-applies while mutter has the session disabled (ok),
/// then the MonitorsChanged path re-applies on the re-enabled session, which
/// would leave the edge dead. So on any failure, disable the session (it
/// keeps its permission: no new prompt) and set them again. Returns
/// `(failed, disabled)`; if `disabled`, the caller must enable the session
/// again.
async fn set_barriers(
    input_capture: &InputCapture,
    session: &Session<InputCapture>,
    barriers: &[Barrier],
    zone_set: u32,
) -> Result<(Vec<BarrierID>, bool), ashpd::Error> {
    let response = input_capture
        .set_pointer_barriers(session, barriers, zone_set, Default::default())
        .await?
        .response()?;
    log::debug!("{response:?}");
    if response.failed_barriers().is_empty() {
        return Ok((vec![], false));
    }
    log::info!(
        "barriers {:?} refused (session enabled?) -- disabling the session and setting them again",
        response.failed_barriers()
    );
    if let Err(e) = input_capture.disable(session, Default::default()).await {
        log::info!("disable before re-setting barriers: {e}");
    }
    let response = input_capture
        .set_pointer_barriers(session, barriers, zone_set, Default::default())
        .await?
        .response()?;
    log::debug!("{response:?}");
    Ok((response.failed_barriers().to_vec(), true))
}

/// "Zones changed" without the geometry changing is common under GNOME: a
/// monitor that briefly drops hot-plug -- e.g. a TV switched to another input
/// and back -- makes mutter re-announce identical zones. Tearing the session
/// down for that is expensive *visibly*: on GNOME every new InputCapture
/// session asks the user for permission again (the portal has no restore
/// token for capture). So if the geometry is unchanged, re-apply the very same
/// barriers (same ids, so the activation -> client mapping still holds)
/// against the new zone set and keep the session.
///
/// No zones at all means the only monitor went away (a TV turned off drops
/// hot-plug for as long as it is off): keep the session and wait for the
/// monitor to return, which arrives as another zones change.
///
/// Returns true if the session was kept; false means "recreate as before".
async fn reapply_if_zones_unchanged(
    input_capture: &InputCapture,
    session: &Session<InputCapture>,
    active: &ActiveBarriers,
) -> bool {
    let Some((geometry, barriers)) = active.borrow().clone() else {
        log::info!("zones changed with no barriers recorded -- recreating session");
        return false;
    };
    let zones = match input_capture
        .zones(session, Default::default())
        .await
        .and_then(|r| r.response())
    {
        Ok(zones) => zones,
        Err(e) => {
            log::warn!("zones changed; could not re-read zones ({e}) -- recreating session");
            return false;
        }
    };
    let new_geometry = zone_geometry(&zones);
    if new_geometry.is_empty() {
        log::info!(
            "zones changed to none (no monitor, e.g. the TV is off) -- keeping capture session"
        );
        return true;
    }
    if new_geometry != geometry {
        log::info!(
            "zones changed (new geometry {geometry:?} -> {new_geometry:?}) -- recreating capture session"
        );
        return false;
    }
    let ashpd_barriers: Vec<Barrier> = barriers.iter().copied().map(|b| b.into()).collect();
    match set_barriers(input_capture, session, &ashpd_barriers, zones.zone_set()).await {
        Ok((failed, disabled)) if failed.is_empty() => {
            if disabled {
                if let Err(e) = input_capture.enable(session, Default::default()).await {
                    log::warn!(
                        "re-enabling after re-setting barriers failed ({e}) -- recreating session"
                    );
                    return false;
                }
            }
            log::info!(
                "zones changed but geometry is identical -- re-applied barriers, kept capture session"
            );
            true
        }
        Ok((failed, _)) => {
            log::warn!("re-applying barriers failed for {failed:?} -- recreating session");
            false
        }
        Err(e) => {
            log::warn!("re-applying barriers failed ({e}) -- recreating session");
            false
        }
    }
}

/// Wait for a reason to update (or end) the capture session.
async fn wait_for_session_update(
    input_capture: &InputCapture,
    session: Option<&Session<InputCapture>>,
    active: &ActiveBarriers,
    cancellation_token: &CancellationToken,
    cancel_update: &CancellationToken,
    zones_changed: &mut (impl Stream<Item = ashpd::desktop::input_capture::ZonesChanged> + Unpin),
    capture_event: &mut Receiver<LibeiNotifyEvent>,
) -> SessionUpdate {
    loop {
        tokio::select! {
            _ = cancellation_token.cancelled() => {
                log::debug!("cancelled");
                return SessionUpdate::None;
            } /* exit requested */
            _ = cancel_update.cancelled() => {
                log::debug!("update task cancelled");
                return SessionUpdate::None;
            } /* session exited */
            _ = zones_changed.next() => {
                log::info!("zones changed (session {})", if session.is_some() { "active" } else { "idle" });
                if let Some(session) = session {
                    if reapply_if_zones_unchanged(input_capture, session, active).await {
                        continue;
                    }
                }
                return SessionUpdate::ZonesChanged;
            } /* zones have changed */
            e = capture_event.recv() => { /* clients changed */
                return match e {
                    Some(e) => {
                        log::debug!("capture event: {e:?}");
                        SessionUpdate::Clients(e)
                    }
                    None => SessionUpdate::None,
                };
            }
        }
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
) -> Result<
    (
        Vec<ICBarrier>,
        HashMap<BarrierID, Position>,
        Vec<ZoneGeometry>,
    ),
    ashpd::Error,
> {
    let zones = input_capture
        .zones(session, Default::default())
        .await?
        .response()?;
    log::debug!("zones: {zones:?}");

    let (barriers, id_map) = select_barriers(&zones, active_clients, next_barrier_id);
    log::debug!("barriers: {barriers:?}");
    log::debug!("client for barrier id: {id_map:?}");

    let ashpd_barriers: Vec<Barrier> = barriers.iter().copied().map(|b| b.into()).collect();
    /* a session left disabled is fine here: every caller enables it next */
    let (failed, _) =
        set_barriers(input_capture, session, &ashpd_barriers, zones.zone_set()).await?;
    if !failed.is_empty() {
        log::warn!("barriers {failed:?} refused even with the session disabled");
    }
    Ok((barriers, id_map, zone_geometry(&zones)))
}

async fn create_session(
    input_capture: &InputCapture,
) -> std::result::Result<(Session<InputCapture>, BitFlags<Capabilities>), ashpd::Error> {
    log::info!("creating input capture session (GNOME will ask for permission)");
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
            .ok_or(CaptureError::EndOfStream)??;
        log::trace!("from ei: {ei_event:?}");
        let client = current_pos.get();
        handle_ei_event(ei_event, client, &context, &event_tx, &release_session).await?;
    }
}

impl LibeiInputCapture {
    pub async fn new() -> std::result::Result<Self, LibeiCaptureCreationError> {
        let input_capture = Box::pin(InputCapture::new().await?);
        let input_capture_ptr = input_capture.as_ref().get_ref() as *const InputCapture;
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
            terminated: false,
        };

        Ok(producer)
    }
}

async fn do_capture(
    input_capture: *const InputCapture,
    mut capture_event: Receiver<LibeiNotifyEvent>,
    notify_release: Arc<Notify>,
    session: Option<(Session<InputCapture>, BitFlags<Capabilities>)>,
    event_tx: Sender<(Position, CaptureEvent)>,
    cancellation_token: CancellationToken,
) -> Result<(), CaptureError> {
    let mut session = session.map(|s| s.0);

    /* safety: libei_task does not outlive Self */
    let input_capture = unsafe { &*input_capture };
    let mut active_clients: Vec<Position> = vec![];
    let mut next_barrier_id = NonZeroU32::new(1).expect("id must be non-zero");

    let mut zones_changed = input_capture.receive_zones_changed().await?;

    let active_barriers: ActiveBarriers = RefCell::new(None);

    loop {
        // do capture session
        let cancel_session = CancellationToken::new();
        let cancel_update = CancellationToken::new();

        let update = if !active_clients.is_empty() {
            // create session
            let session = match session.take() {
                Some(s) => s,
                None => create_session(input_capture).await?.0,
            };

            let capture_session = do_capture_session(
                input_capture,
                &session,
                &event_tx,
                &active_clients,
                &mut next_barrier_id,
                &notify_release,
                &active_barriers,
                (cancel_session.clone(), cancel_update.clone()),
            );

            // kill session if clients need to be updated (zone changes with
            // unchanged geometry are absorbed without killing it)
            let handle_session_update_request = async {
                let update = wait_for_session_update(
                    input_capture,
                    Some(&session),
                    &active_barriers,
                    &cancellation_token,
                    &cancel_update,
                    &mut zones_changed,
                    &mut capture_event,
                )
                .await;
                // kill session (might already be dead!)
                log::debug!("=> cancelling session");
                cancel_session.cancel();
                update
            };

            let (capture_result, update) =
                tokio::join!(capture_session, handle_session_update_request);
            log::debug!("capture session + session_update task done!");
            active_barriers.replace(None);

            // disable capture
            log::debug!("disabling input capture");
            if let Err(e) = input_capture.disable(&session, Default::default()).await {
                log::warn!("input_capture.disable(&session) {e}");
            }
            if let Err(e) = session.close().await {
                log::warn!("session.close(): {e}");
            }

            // propagate error from capture session
            capture_result?;
            update
        } else {
            wait_for_session_update(
                input_capture,
                None,
                &active_barriers,
                &cancellation_token,
                &cancel_update,
                &mut zones_changed,
                &mut capture_event,
            )
            .await
        };
        if let SessionUpdate::ZonesChanged = update {
            log::debug!("zones changed: session will be recreated");
        }

        // update clients if requested
        if let SessionUpdate::Clients(event) = update {
            match event {
                LibeiNotifyEvent::Create(p) => active_clients.push(p),
                LibeiNotifyEvent::Destroy(p) => active_clients.retain(|&pos| pos != p),
            }
        }

        // break
        if cancellation_token.is_cancelled() {
            break Ok(());
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn do_capture_session(
    input_capture: &InputCapture,
    session: &Session<InputCapture>,
    event_tx: &Sender<(Position, CaptureEvent)>,
    active_clients: &[Position],
    next_barrier_id: &mut NonZeroU32,
    notify_release: &Notify,
    active_barriers: &ActiveBarriers,
    cancel: (CancellationToken, CancellationToken),
) -> Result<(), CaptureError> {
    let (cancel_session, cancel_update) = cancel;
    // current client
    let current_pos = Rc::new(Cell::new(None));

    // connect to eis server
    let (context, _conn, ei_event_stream) = connect_to_eis(input_capture, session).await?;

    // set barriers
    let (mut barriers, mut pos_for_barrier_id, geometry) =
        update_barriers(input_capture, session, active_clients, next_barrier_id).await?;
    active_barriers.replace(Some((geometry, barriers.clone())));

    log::debug!("enabling session");
    input_capture.enable(session, Default::default()).await?;

    // cancellation token to release session
    let release_session = Arc::new(Notify::new());

    // async event task
    let cancel_ei_handler = CancellationToken::new();
    let event_chan = event_tx.clone();
    let pos = current_pos.clone();
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
                pos,
            ) => {
                log::info!("session end: libei event stream exited ({r:?})");
                cancel_session_clone.cancel();
            }
            _ = cancel_ei_handler_clone.cancelled() => {},
        }
        Ok::<(), CaptureError>(())
    };

    let capture_session_task = async {
        // receiver for activation tokens
        let mut activated = input_capture.receive_activated().await?;
        // the compositor disabling the session (we only disable it ourselves
        // after this loop): GNOME does this on monitor changes
        let mut disabled = input_capture.receive_disabled().await?;
        let session_dbg = format!("{session:?}"); // Session::path() is crate-private
        let mut monitors = mutter_monitors_changed().await;
        let mut ei_devices_changed = false;
        let mut compositor_disabled = false;
        let mut monitors_changed = false;
        loop {
            tokio::select! {
                activated = activated.next() => {
                    let activated = activated.ok_or_else(|| {
                        log::info!("session end: activation stream closed");
                        CaptureError::ActivationClosed
                    })?;
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
                            log::info!("session end: ei devices changed (while capturing)");
                            ei_devices_changed = true;
                        },
                        _ = cancel_session.cancelled() => { /* kill session notify */
                            log::info!("session end: cancel requested (while capturing)");
                            break
                        },
                        d = disabled.next() => {
                            if let Some(d) = d {
                                let ours = session_dbg.contains(d.session_handle().as_str());
                                log::info!("Disabled signal (while capturing) for {} -- ours: {ours} ({session_dbg})", d.session_handle().as_str());
                                compositor_disabled |= ours;
                            }
                        },
                        _ = next_monitors_changed(&mut monitors) => {
                            log::info!("Mutter MonitorsChanged (while capturing)");
                            monitors_changed = true;
                        },
                    }

                    // a disabled session has nothing to release; after a
                    // monitor change releasing may fail -- not fatal then
                    if !compositor_disabled {
                        if let Err(e) = release_capture(input_capture, session, activated, pos).await {
                            if monitors_changed {
                                log::info!("release after monitor change failed: {e}");
                            } else {
                                return Err(e);
                            }
                        }
                    }

                }
                _ = notify_release.notified() => { /* capture release -> we are not capturing anyway, so ignore */
                    log::debug!("release session requested");
                },
                _ = release_session.notified() => { /* release session */
                    log::info!("session end: ei devices changed");
                    ei_devices_changed = true;
                },
                _ = cancel_session.cancelled() => { /* kill session notify */
                    log::info!("session end: cancel requested");
                    break
                },
                d = disabled.next() => {
                    if let Some(d) = d {
                        let ours = session_dbg.contains(d.session_handle().as_str());
                        log::info!("Disabled signal for {} -- ours: {ours} ({session_dbg})", d.session_handle().as_str());
                        compositor_disabled |= ours;
                    }
                },
                _ = next_monitors_changed(&mut monitors) => {
                    log::info!("Mutter MonitorsChanged");
                    monitors_changed = true;
                },
            }
            if monitors_changed {
                settle_monitors(&mut monitors).await;
            }
            if ei_devices_changed || compositor_disabled || monitors_changed {
                let why = if compositor_disabled {
                    "compositor disabled the session"
                } else if ei_devices_changed {
                    "ei devices changed"
                } else {
                    "monitors changed"
                };
                monitors_changed = false;
                if *REENABLE_ON_DEVICE_CHANGE {
                    ei_devices_changed = false;
                    compositor_disabled = false;
                    match reapply_and_enable(
                        input_capture,
                        session,
                        active_clients,
                        next_barrier_id,
                    )
                    .await
                    {
                        Ok((b, m, geometry)) => {
                            /* no zones (the monitor is gone): keep the
                             * barriers recorded for the real geometry, the
                             * zones-changed path re-applies them on return */
                            if !geometry.is_empty() {
                                active_barriers.replace(Some((geometry, b.clone())));
                                barriers = b;
                                pos_for_barrier_id = m;
                            }
                            log::info!(
                                "{why} -- re-applied barriers and re-enabled the same session (no new permission prompt)"
                            );
                            continue;
                        }
                        Err(e) => {
                            log::warn!("{why}; re-enabling failed ({e}) -- recreating the session")
                        }
                    }
                } else if !ei_devices_changed {
                    // without the re-enable option, a disabled session is dead:
                    // recreate it (this is what used to fail silently)
                    log::info!("{why} -- recreating the session");
                }
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
            log::info!("ei seat added");
            s.seat.bind_capabilities(all_capabilities);
            context.flush().map_err(|e| io::Error::new(e.kind(), e))?;
        }
        EiEvent::SeatRemoved(_) => {
            log::info!("ei seat removed -> releasing session");
            release_session.notify_waiters();
        }
        EiEvent::DeviceAdded(_) => log::info!("ei device added"),
        EiEvent::DeviceRemoved(_) => {
            if *RESTART_SESSION_ON_DEVICE_CHANGE {
                log::info!("ei device removed -> releasing session (GNOME restart rule)");
                release_session.notify_waiters();
            } else {
                log::debug!("ignoring device change: {ei_event:?}");
            }
        }
        EiEvent::DevicePaused(_) | EiEvent::DeviceResumed(_) => {}
        EiEvent::DeviceStartEmulating(_) => log::debug!("START EMULATING"),
        EiEvent::DeviceStopEmulating(_) => log::debug!("STOP EMULATING"),
        EiEvent::Disconnected(d) => {
            log::info!("ei disconnected: {:?}", d.reason);
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
