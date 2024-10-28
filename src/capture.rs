use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

use futures::StreamExt;
use input_capture::{
    CaptureError, CaptureEvent, CaptureHandle, InputCapture, InputCaptureError, Position,
};
use lan_mouse_ipc::ClientHandle;
use lan_mouse_proto::ProtoEvent;
use local_channel::mpsc::{channel, Receiver, Sender};
use tokio::{
    process::Command,
    task::{spawn_local, JoinHandle},
};

use crate::{connect::LanMouseConnection, service::Service};

pub(crate) struct Capture {
    _active: Rc<Cell<Option<CaptureHandle>>>,
    tx: Sender<CaptureRequest>,
    task: JoinHandle<()>,
    event_rx: Receiver<ICaptureEvent>,
}

pub(crate) enum ICaptureEvent {
    /// a client was entered
    ClientEntered(CaptureHandle),
    /// capture disabled
    CaptureDisabled,
    /// capture disabled
    CaptureEnabled,
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
        let (tx, rx) = channel();
        let (event_tx, event_rx) = channel();
        let active = Rc::new(Cell::new(None));
        let task = spawn_local(Self::run(
            active.clone(),
            service,
            backend,
            rx,
            conn,
            event_tx,
        ));
        Self {
            _active: active,
            tx,
            task,
            event_rx,
        }
    }

    pub(crate) fn reenable(&self) {
        self.tx
            .send(CaptureRequest::Reenable)
            .expect("channel closed");
    }

    pub(crate) async fn terminate(&mut self) {
        self.tx
            .send(CaptureRequest::Terminate)
            .expect("channel closed");
        log::debug!("terminating capture");
        if let Err(e) = (&mut self.task).await {
            log::warn!("{e}");
        }
    }

    pub(crate) fn create(&self, handle: CaptureHandle, pos: lan_mouse_ipc::Position) {
        self.tx
            .send(CaptureRequest::Create(handle, to_capture_pos(pos)))
            .expect("channel closed");
    }

    pub(crate) fn destroy(&self, handle: CaptureHandle) {
        self.tx
            .send(CaptureRequest::Destroy(handle))
            .expect("channel closed");
    }

    pub(crate) fn release(&self) {
        self.tx
            .send(CaptureRequest::Release)
            .expect("channel closed");
    }

    pub(crate) async fn event(&mut self) -> ICaptureEvent {
        self.event_rx.recv().await.expect("channel closed")
    }

    async fn run(
        active: Rc<Cell<Option<CaptureHandle>>>,
        service: Service,
        backend: Option<input_capture::Backend>,
        mut rx: Receiver<CaptureRequest>,
        mut conn: LanMouseConnection,
        mut event_tx: Sender<ICaptureEvent>,
    ) {
        loop {
            if let Err(e) = do_capture(
                &active,
                &service,
                backend,
                &mut conn,
                &mut rx,
                &mut event_tx,
            )
            .await
            {
                log::warn!("input capture exited: {e}");
            }
            event_tx
                .send(ICaptureEvent::CaptureDisabled)
                .expect("channel closed");
            loop {
                match rx.recv().await.expect("channel closed") {
                    CaptureRequest::Reenable => break,
                    CaptureRequest::Terminate => return,
                    _ => {}
                }
            }
        }
    }
}

async fn do_capture(
    active: &Cell<Option<CaptureHandle>>,
    service: &Service,
    backend: Option<input_capture::Backend>,
    conn: &mut LanMouseConnection,
    rx: &mut Receiver<CaptureRequest>,
    event_tx: &mut Sender<ICaptureEvent>,
) -> Result<(), InputCaptureError> {
    /* allow cancelling capture request */
    let mut capture = tokio::select! {
        r = InputCapture::new(backend) => r?,
        _ = wait_for_termination(rx) => return Ok(()),
    };
    event_tx
        .send(ICaptureEvent::CaptureEnabled)
        .expect("channel closed");

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
    for (handle, pos) in clients {
        capture.create(handle, to_capture_pos(pos)).await?;
    }

    let mut state = State::WaitingForAck;

    loop {
        tokio::select! {
            event = capture.next() => match event {
                Some(event) => handle_capture_event(active, &service, &mut capture, conn, event?, &mut state, event_tx).await?,
                None => return Ok(()),
            },
            (handle, event) = conn.recv() => {
                if let Some(active) = active.get() {
                    if handle != active {
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
                        release_capture(&mut capture, &active).await?;
                    },
                    _ => {}
                }
            },
            e = rx.recv() => match e.expect("channel closed") {
                CaptureRequest::Reenable => { /* already active */ },
                CaptureRequest::Release => release_capture(&mut capture, &active).await?,
                CaptureRequest::Create(h, p) => capture.create(h, p).await?,
                CaptureRequest::Destroy(h) => capture.destroy(h).await?,
                CaptureRequest::Terminate => break,
            }
        }
    }

    capture.terminate().await?;
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
    active: &Cell<Option<CaptureHandle>>,
    service: &Service,
    capture: &mut InputCapture,
    conn: &LanMouseConnection,
    event: (CaptureHandle, CaptureEvent),
    state: &mut State,
    event_tx: &mut Sender<ICaptureEvent>,
) -> Result<(), CaptureError> {
    let (handle, event) = event;
    log::trace!("({handle}): {event:?}");

    if capture.keys_pressed(&service.config.release_bind) {
        log::info!("releasing capture: release-bind pressed");
        return release_capture(capture, &active).await;
    }

    if event == CaptureEvent::Begin {
        event_tx
            .send(ICaptureEvent::ClientEntered(handle))
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
    if event == CaptureEvent::Begin && Some(handle) != active.get() {
        *state = State::WaitingForAck;
        active.replace(Some(handle));
        log::info!("entering client {handle} ...");
        spawn_hook_command(service, handle);
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
            _ => ProtoEvent::Input(e),
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
    active: &Cell<Option<CaptureHandle>>,
) -> Result<(), CaptureError> {
    active.replace(None);
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

fn spawn_hook_command(service: &Service, handle: ClientHandle) {
    let Some(cmd) = service.client_manager.get_enter_cmd(handle) else {
        return;
    };
    tokio::task::spawn_local(async move {
        log::info!("spawning command!");
        let mut child = match Command::new("sh").arg("-c").arg(cmd.as_str()).spawn() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("could not execute cmd: {e}");
                return;
            }
        };
        match child.wait().await {
            Ok(s) => {
                if s.success() {
                    log::info!("{cmd} exited successfully");
                } else {
                    log::warn!("{cmd} exited with {s}");
                }
            }
            Err(e) => log::warn!("{cmd}: {e}"),
        }
    });
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
