use std::{
    cell::Cell,
    time::{Duration, Instant},
};

use futures::StreamExt;
use input_capture::{
    CaptureError, CaptureEvent, CaptureHandle, InputCapture, InputCaptureError, Position,
};
use lan_mouse_ipc::{ClientHandle, Status};
use lan_mouse_proto::ProtoEvent;
use local_channel::mpsc::{channel, Receiver, Sender};
use tokio::{
    process::Command,
    task::{spawn_local, JoinHandle},
};

use crate::{connect::LanMouseConnection, service::Service};

pub(crate) struct Capture {
    tx: Sender<CaptureRequest>,
    task: JoinHandle<()>,
}

#[derive(Clone, Copy, Debug)]
enum CaptureRequest {
    /// capture must release the mouse
    Release,
    /// add a capture client
    Create(CaptureHandle, Position),
    /// destory a capture client
    Destroy(CaptureHandle),
}

impl Capture {
    pub(crate) fn new(server: Service, conn: LanMouseConnection) -> Self {
        let (tx, rx) = channel();
        let task = spawn_local(Self::run(server.clone(), rx, conn));
        Self { tx, task }
    }

    pub(crate) async fn terminate(&mut self) {
        log::debug!("terminating capture");
        self.tx.close();
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

    #[allow(unused)]
    pub(crate) fn release(&self) {
        self.tx
            .send(CaptureRequest::Release)
            .expect("channel closed");
    }

    async fn run(server: Service, mut rx: Receiver<CaptureRequest>, mut conn: LanMouseConnection) {
        loop {
            if let Err(e) = do_capture(&server, &mut conn, &mut rx).await {
                log::warn!("input capture exited: {e}");
            }
            server.set_capture_status(Status::Disabled);
            loop {
                tokio::select! {
                    e = rx.recv() => match e {
                        Some(_) => continue,
                        None => break,
                    },
                    _ = server.capture_enabled() => break,
                    _ = server.cancelled() => return,
                }
            }
        }
    }
}

async fn do_capture(
    server: &Service,
    conn: &mut LanMouseConnection,
    rx: &mut Receiver<CaptureRequest>,
) -> Result<(), InputCaptureError> {
    let backend = server.config.capture_backend.map(|b| b.into());

    /* allow cancelling capture request */
    let mut capture = tokio::select! {
        r = InputCapture::new(backend) => r?,
        _ = server.cancelled() => return Ok(()),
    };
    server.set_capture_status(Status::Enabled);

    let clients = server.client_manager.active_clients();
    let clients = clients.iter().copied().map(|handle| {
        (
            handle,
            server
                .client_manager
                .get_pos(handle)
                .expect("no such client"),
        )
    });

    /* create barriers for active clients */
    for (handle, pos) in clients {
        capture.create(handle, to_capture_pos(pos)).await?;
    }

    let mut state = State::Receiving;

    loop {
        tokio::select! {
            event = capture.next() => match event {
                Some(event) => handle_capture_event(server, &mut capture, conn, event?, &mut state).await?,
                None => return Ok(()),
            },
            (handle, event) = conn.recv() => {
                if let Some(active) = server.get_active() {
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
                    ProtoEvent::Leave(_) => release_capture(&mut capture, server).await?,
                    _ => {}
                }
            },
            e = rx.recv() => {
                match e {
                    Some(e) => match e {
                        CaptureRequest::Release => release_capture(&mut capture, server).await?,
                        CaptureRequest::Create(h, p) => capture.create(h, p).await?,
                        CaptureRequest::Destroy(h) => capture.destroy(h).await?,
                    },
                    None => break,
                }
            }
            _ = server.cancelled() => break,
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
    Receiving,
    WaitingForAck,
    Sending,
}

async fn handle_capture_event(
    server: &Service,
    capture: &mut InputCapture,
    conn: &LanMouseConnection,
    event: (CaptureHandle, CaptureEvent),
    state: &mut State,
) -> Result<(), CaptureError> {
    let (handle, event) = event;
    log::trace!("({handle}): {event:?}");

    if server.should_release.borrow_mut().take().is_some() && *state != State::Receiving {
        log::info!("releasing capture: a client entered the device");
        *state = State::Receiving;
        return release_capture(capture, server).await;
    }

    if capture.keys_pressed(&server.config.release_bind) {
        log::info!("releasing capture: release-bind pressed");
        return release_capture(capture, server).await;
    }

    // activated a new client
    if event == CaptureEvent::Begin && Some(handle) != server.get_active() {
        *state = State::WaitingForAck;
        server.set_active(Some(handle));
        log::info!("entering client {handle} ...");
        spawn_hook_command(server, handle);
    }

    let pos = match server.client_manager.get_pos(handle) {
        Some(pos) => to_proto_pos(pos.opposite()),
        None => return release_capture(capture, server).await,
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

async fn release_capture(capture: &mut InputCapture, server: &Service) -> Result<(), CaptureError> {
    server.set_active(None);
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

fn spawn_hook_command(server: &Service, handle: ClientHandle) {
    let Some(cmd) = server.client_manager.get_enter_cmd(handle) else {
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
