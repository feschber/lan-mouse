use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use futures::StreamExt;
use input_capture::{
    CaptureError, CaptureEvent, CaptureHandle, InputCapture, InputCaptureError, Position,
};
use lan_mouse_proto::ProtoEvent;
use local_channel::mpsc::{channel, Receiver, Sender};
use tokio::task::{spawn_local, JoinHandle};

use crate::{connect::LanMouseConnection, service::Service};

pub(crate) struct Capture {
    exit_requested: Rc<Cell<bool>>,
    request_tx: Sender<CaptureRequest>,
    task: JoinHandle<()>,
    event_rx: Receiver<ICaptureEvent>,
}

pub(crate) enum ICaptureEvent {
    /// a client was entered
    CaptureBegin(CaptureHandle),
    /// capture disabled
    CaptureDisabled,
    /// capture disabled
    CaptureEnabled,
    /// A (new) client was entered.
    /// In contrast to [`ICaptureEvent::CaptureBegin`] this
    /// event is only triggered when the capture was
    /// explicitly released in the meantime by
    /// either the remote client leaving its device region,
    /// a new device entering the screen or the release bind.
    ClientEntered(u64),
}

#[derive(Clone, Copy, Debug)]
enum CaptureRequest {
    /// capture must release the mouse
    Release,
    /// add a capture client
    Create(CaptureHandle, Position),
    /// destory a capture client
    Destroy(CaptureHandle),
    /// terminate
    Terminate,
    /// reenable input capture
    Reenable,
}

impl Capture {
    pub(crate) fn new(
        backend: Option<input_capture::Backend>,
        conn: LanMouseConnection,
        service: Service,
    ) -> Self {
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let exit_requested = Rc::new(Cell::new(false));
        let task = spawn_local(Self::run(
            exit_requested.clone(),
            service,
            backend,
            request_rx,
            conn,
            event_tx,
        ));
        Self {
            exit_requested,
            request_tx,
            task,
            event_rx,
        }
    }

    pub(crate) fn reenable(&self) {
        self.request_tx
            .send(CaptureRequest::Reenable)
            .expect("channel closed");
    }

    pub(crate) async fn terminate(&mut self) {
        self.exit_requested.replace(true);
        self.request_tx
            .send(CaptureRequest::Terminate)
            .expect("channel closed");
        log::debug!("terminating capture");
        if let Err(e) = (&mut self.task).await {
            log::warn!("{e}");
        }
    }

    pub(crate) fn create(&self, handle: CaptureHandle, pos: lan_mouse_ipc::Position) {
        self.request_tx
            .send(CaptureRequest::Create(handle, to_capture_pos(pos)))
            .expect("channel closed");
    }

    pub(crate) fn destroy(&self, handle: CaptureHandle) {
        self.request_tx
            .send(CaptureRequest::Destroy(handle))
            .expect("channel closed");
    }

    pub(crate) fn release(&self) {
        self.request_tx
            .send(CaptureRequest::Release)
            .expect("channel closed");
    }

    pub(crate) async fn event(&mut self) -> ICaptureEvent {
        self.event_rx.recv().await.expect("channel closed")
    }

    async fn run(
        exit_requested: Rc<Cell<bool>>,
        service: Service,
        backend: Option<input_capture::Backend>,
        mut request_rx: Receiver<CaptureRequest>,
        mut conn: LanMouseConnection,
        mut event_tx: Sender<ICaptureEvent>,
    ) {
        let mut active = None;
        loop {
            if let Err(e) = do_capture(
                &mut active,
                &service,
                backend,
                &mut conn,
                &mut request_rx,
                &mut event_tx,
            )
            .await
            {
                log::warn!("input capture exited: {e}");
            }
            if exit_requested.get() {
                break;
            }
            loop {
                match request_rx.recv().await.expect("channel closed") {
                    CaptureRequest::Reenable => break,
                    CaptureRequest::Terminate => return,
                    _ => {}
                }
            }
        }
    }
}

async fn do_capture(
    active: &mut Option<CaptureHandle>,
    service: &Service,
    backend: Option<input_capture::Backend>,
    conn: &mut LanMouseConnection,
    request_rx: &mut Receiver<CaptureRequest>,
    event_tx: &mut Sender<ICaptureEvent>,
) -> Result<(), InputCaptureError> {
    /* allow cancelling capture request */
    let mut capture = tokio::select! {
        r = InputCapture::new(backend) => r?,
        _ = wait_for_termination(request_rx) => return Ok(()),
    };

    let _capture_guard = DropGuard::new(
        event_tx,
        ICaptureEvent::CaptureEnabled,
        ICaptureEvent::CaptureDisabled,
    );

    let clients = service.client_manager.active_clients();
    let clients = clients.iter().copied().map(|handle| {
        (
            handle,
            service
                .client_manager
                .get_pos(handle)
                .expect("no such client"),
        )
    });

    /* create barriers for active clients */
    let r = create_clients(&mut capture, clients, request_rx).await;
    if let Err(e) = r {
        capture.terminate().await?;
        return Err(e.into());
    }

    let r = do_capture_session(active, &mut capture, conn, event_tx, request_rx, service).await;

    // FIXME replace with async drop when stabilized
    capture.terminate().await?;

    r
}

async fn create_clients(
    capture: &mut InputCapture,
    clients: impl Iterator<Item = (CaptureHandle, lan_mouse_ipc::Position)>,
    request_rx: &mut Receiver<CaptureRequest>,
) -> Result<(), CaptureError> {
    for (handle, pos) in clients {
        tokio::select! {
            r = capture.create(handle, to_capture_pos(pos)) => r?,
            _ = wait_for_termination(request_rx) => return Ok(()),
        }
    }
    Ok(())
}

async fn do_capture_session(
    active: &mut Option<CaptureHandle>,
    capture: &mut InputCapture,
    conn: &mut LanMouseConnection,
    event_tx: &Sender<ICaptureEvent>,
    request_rx: &mut Receiver<CaptureRequest>,
    service: &Service,
) -> Result<(), InputCaptureError> {
    let mut state = State::WaitingForAck;

    loop {
        tokio::select! {
            event = capture.next() => match event {
                Some(event) => handle_capture_event(active, service, capture, conn, event?, &mut state, event_tx).await?,
                None => return Ok(()),
            },
            (handle, event) = conn.recv() => {
                if let Some(active) = active {
                    if handle != *active {
                        // we only care about events coming from the client we are currently connected to
                        // only `Ack` and `Leave` are relevant
                        continue
                    }
                }

                match event {
                    // connection acknowlegded => set state to Sending
                    ProtoEvent::Ack(_) => {
                        log::info!("client {handle} acknowledged the connection!");
                        state = State::Sending;
                    }
                    // client disconnected
                    ProtoEvent::Leave(_) => {
                        log::info!("releasing capture: left remote client device region");
                        release_capture(capture, active).await?;
                    },
                    _ => {}
                }
            },
            e = request_rx.recv() => match e.expect("channel closed") {
                CaptureRequest::Reenable => { /* already active */ },
                CaptureRequest::Release => release_capture(capture, active).await?,
                CaptureRequest::Create(h, p) => capture.create(h, p).await?,
                CaptureRequest::Destroy(h) => capture.destroy(h).await?,
                CaptureRequest::Terminate => break,
            }
        }
    }
    Ok(())
}

thread_local! {
    static PREV_LOG: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// debounce a statement `$st`, i.e. the statement is executed only if the
/// time since the previous execution is at least `$dur`.
/// `$prev` is used to keep track of this timestamp
macro_rules! debounce {
    ($prev:ident, $dur:expr, $st:stmt) => {
        let exec = match $prev.get() {
            None => true,
            Some(instant) if instant.elapsed() > $dur => true,
            _ => false,
        };
        if exec {
            $prev.replace(Some(Instant::now()));
            $st
        }
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    WaitingForAck,
    Sending,
}

async fn handle_capture_event(
    active: &mut Option<CaptureHandle>,
    service: &Service,
    capture: &mut InputCapture,
    conn: &LanMouseConnection,
    event: (CaptureHandle, CaptureEvent),
    state: &mut State,
    event_tx: &Sender<ICaptureEvent>,
) -> Result<(), CaptureError> {
    let (handle, event) = event;
    log::trace!("({handle}): {event:?}");

    if capture.keys_pressed(&service.config.release_bind) {
        log::info!("releasing capture: release-bind pressed");
        return release_capture(capture, active).await;
    }

    if event == CaptureEvent::Begin {
        event_tx
            .send(ICaptureEvent::CaptureBegin(handle))
            .expect("channel closed");
    }

    // incoming connection
    if handle >= Service::ENTER_HANDLE_BEGIN {
        // if there is no active outgoing connection at the current capture,
        // we release the capture
        if let Some(pos) = service.get_incoming_pos(handle) {
            if service.client_manager.client_at(pos).is_none() {
                log::info!("releasing capture: no active client at this position");
                capture.release().await?;
            }
        }
        // we dont care about events from incoming handles except for releasing the capture
        return Ok(());
    }

    // activated a new client
    if event == CaptureEvent::Begin && Some(handle) != *active {
        *state = State::WaitingForAck;
        active.replace(handle);
        event_tx
            .send(ICaptureEvent::ClientEntered(handle))
            .expect("channel closed");
    }

    let pos = match service.client_manager.get_pos(handle) {
        Some(pos) => to_proto_pos(pos.opposite()),
        None => return release_capture(capture, active).await,
    };

    let event = match event {
        CaptureEvent::Begin => ProtoEvent::Enter(pos),
        CaptureEvent::Input(e) => match state {
            // connection not acknowledged, repeat `Enter` event
            State::WaitingForAck => ProtoEvent::Enter(pos),
            State::Sending => ProtoEvent::Input(e),
        },
    };

    if let Err(e) = conn.send(event, handle).await {
        const DUR: Duration = Duration::from_millis(500);
        debounce!(PREV_LOG, DUR, log::warn!("releasing capture: {e}"));
        capture.release().await?;
    }
    Ok(())
}

async fn release_capture(
    capture: &mut InputCapture,
    active: &mut Option<CaptureHandle>,
) -> Result<(), CaptureError> {
    active.take();
    capture.release().await
}

fn to_capture_pos(pos: lan_mouse_ipc::Position) -> input_capture::Position {
    match pos {
        lan_mouse_ipc::Position::Left => input_capture::Position::Left,
        lan_mouse_ipc::Position::Right => input_capture::Position::Right,
        lan_mouse_ipc::Position::Top => input_capture::Position::Top,
        lan_mouse_ipc::Position::Bottom => input_capture::Position::Bottom,
    }
}

fn to_proto_pos(pos: lan_mouse_ipc::Position) -> lan_mouse_proto::Position {
    match pos {
        lan_mouse_ipc::Position::Left => lan_mouse_proto::Position::Left,
        lan_mouse_ipc::Position::Right => lan_mouse_proto::Position::Right,
        lan_mouse_ipc::Position::Top => lan_mouse_proto::Position::Top,
        lan_mouse_ipc::Position::Bottom => lan_mouse_proto::Position::Bottom,
    }
}

async fn wait_for_termination(rx: &mut Receiver<CaptureRequest>) {
    loop {
        match rx.recv().await.expect("channel closed") {
            CaptureRequest::Terminate => return,
            CaptureRequest::Release => continue,
            CaptureRequest::Create(_, _) => continue,
            CaptureRequest::Destroy(_) => continue,
            CaptureRequest::Reenable => continue,
        }
    }
}

struct DropGuard<'a, T> {
    tx: &'a Sender<T>,
    on_drop: Option<T>,
}

impl<'a, T> DropGuard<'a, T> {
    fn new(tx: &'a Sender<T>, on_new: T, on_drop: T) -> Self {
        tx.send(on_new).expect("channel closed");
        let on_drop = Some(on_drop);
        Self { tx, on_drop }
    }
}

impl<'a, T> Drop for DropGuard<'a, T> {
    fn drop(&mut self) {
        self.tx
            .send(self.on_drop.take().expect("item"))
            .expect("channel closed");
    }
}
