use futures::StreamExt;
use lan_mouse_proto::ProtoEvent;
use local_channel::mpsc::{Receiver, Sender};
use std::net::SocketAddr;

use tokio::{process::Command, task::JoinHandle};

use input_capture::{
    self, CaptureError, CaptureEvent, CaptureHandle, InputCapture, InputCaptureError, Position,
};

use crate::server::State;
use lan_mouse_ipc::{ClientHandle, Status};

use super::Server;

#[derive(Clone, Copy, Debug)]
pub(crate) enum CaptureRequest {
    /// capture must release the mouse
    Release,
    /// add a capture client
    Create(CaptureHandle, Position),
    /// destory a capture client
    Destroy(CaptureHandle),
}

pub(crate) fn new(
    server: Server,
    capture_rx: Receiver<CaptureRequest>,
    udp_send: Sender<(ProtoEvent, SocketAddr)>,
) -> JoinHandle<()> {
    let backend = server.config.capture_backend.map(|b| b.into());
    tokio::task::spawn_local(capture_task(server, backend, udp_send, capture_rx))
}

async fn capture_task(
    server: Server,
    backend: Option<input_capture::Backend>,
    sender_tx: Sender<(ProtoEvent, SocketAddr)>,
    mut notify_rx: Receiver<CaptureRequest>,
) {
    loop {
        if let Err(e) = do_capture(backend, &server, &sender_tx, &mut notify_rx).await {
            log::warn!("input capture exited: {e}");
        }
        server.set_capture_status(Status::Disabled);
        if server.is_cancelled() {
            break;
        }

        // allow cancellation
        loop {
            tokio::select! {
                _ = notify_rx.recv() => continue, /* need to ignore requests here! */
                _ = server.capture_enabled() => break,
                _ = server.cancelled() => return,
            }
        }
    }
}

async fn do_capture(
    backend: Option<input_capture::Backend>,
    server: &Server,
    sender_tx: &Sender<(ProtoEvent, SocketAddr)>,
    notify_rx: &mut Receiver<CaptureRequest>,
) -> Result<(), InputCaptureError> {
    /* allow cancelling capture request */
    let mut capture = tokio::select! {
        r = InputCapture::new(backend) => r?,
        _ = server.cancelled() => return Ok(()),
    };

    server.set_capture_status(Status::Enabled);

    let clients = server.active_clients();
    let clients = clients.iter().copied().map(|handle| {
        (
            handle,
            server
                .client_manager
                .borrow()
                .get(handle)
                .map(|(c, _)| c.pos)
                .expect("no such client"),
        )
    });
    for (handle, pos) in clients {
        capture.create(handle, to_capture_pos(pos)).await?;
    }

    loop {
        tokio::select! {
            event = capture.next() => match event {
                Some(event) => handle_capture_event(server, &mut capture, sender_tx, event?).await?,
                None => return Ok(()),
            },
            e = notify_rx.recv() => {
                log::debug!("input capture notify rx: {e:?}");
                match e {
                    Some(e) => match e {
                        CaptureRequest::Release => {
                            capture.release().await?;
                            server.state.replace(State::Receiving);
                        }
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

async fn handle_capture_event(
    server: &Server,
    capture: &mut InputCapture,
    sender_tx: &Sender<(ProtoEvent, SocketAddr)>,
    event: (CaptureHandle, CaptureEvent),
) -> Result<(), CaptureError> {
    let (handle, event) = event;
    log::trace!("({handle}) {event:?}");

    // capture started
    if event == CaptureEvent::Begin {
        // wait for remote to acknowlegde enter
        server.set_state(State::AwaitAck);
        server.set_active(Some(handle));
        // restart ping timer to release capture if unreachable
        server.restart_ping_timer();
        // spawn enter hook cmd
        spawn_hook_command(server, handle);
    }

    // release capture if emulation set state to Receiveing
    if server.get_state() == State::Receiving {
        capture.release().await?;
        return Ok(());
    }

    // check release bind
    if capture.keys_pressed(&server.release_bind) {
        capture.release().await?;
        server.set_state(State::Receiving);
    }

    if let Some(addr) = server.active_addr(handle) {
        let event = match server.get_state() {
            State::Sending => match event {
                CaptureEvent::Begin => ProtoEvent::Enter(0),
                CaptureEvent::Input(e) => ProtoEvent::Input(e),
            },
            /* send additional enter events until acknowleged */
            State::AwaitAck => ProtoEvent::Enter(0),
            /* released capture */
            State::Receiving => ProtoEvent::Leave(0),
        };
        log::error!("SENDING: {event:?} -> {addr:?}");
        sender_tx.send((event, addr)).expect("sender closed");
    };

    Ok(())
}

fn spawn_hook_command(server: &Server, handle: ClientHandle) {
    let Some(cmd) = server
        .client_manager
        .borrow()
        .get(handle)
        .and_then(|(c, _)| c.cmd.clone())
    else {
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

fn to_capture_pos(pos: lan_mouse_ipc::Position) -> input_capture::Position {
    match pos {
        lan_mouse_ipc::Position::Left => input_capture::Position::Left,
        lan_mouse_ipc::Position::Right => input_capture::Position::Right,
        lan_mouse_ipc::Position::Top => input_capture::Position::Top,
        lan_mouse_ipc::Position::Bottom => input_capture::Position::Bottom,
    }
}
