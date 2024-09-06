use futures::StreamExt;
use input_capture::{
    Backend, CaptureError, CaptureEvent, CaptureHandle, InputCapture, InputCaptureError, Position,
};
use lan_mouse_ipc::{ClientHandle, Status};
use lan_mouse_proto::ProtoEvent;
use local_channel::mpsc::{channel, Receiver, Sender};
use tokio::{
    process::Command,
    task::{spawn_local, JoinHandle},
};

use crate::{connect::LanMouseConnection, server::Server};

pub(crate) struct CaptureProxy {
    server: Server,
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

impl CaptureProxy {
    pub(crate) fn new(server: Server, backend: Option<Backend>, conn: LanMouseConnection) -> Self {
        let (tx, rx) = channel();
        let task = spawn_local(Self::run(server.clone(), backend, rx, conn));
        Self { server, tx, task }
    }

    pub(crate) async fn run(
        server: Server,
        backend: Option<Backend>,
        mut rx: Receiver<CaptureRequest>,
        conn: LanMouseConnection,
    ) {
        loop {
            if let Err(e) = do_capture(backend, &server, &conn, &mut rx).await {
                log::warn!("input capture exited: {e}");
            }
            server.set_capture_status(Status::Disabled);
            tokio::select! {
                _ = rx.recv() => continue,
                _ = server.capture_enabled() => break,
                _ = server.cancelled() => return,
            }
        }
    }
}

async fn do_capture(
    backend: Option<Backend>,
    server: &Server,
    conn: &LanMouseConnection,
    rx: &mut Receiver<CaptureRequest>,
) -> Result<(), InputCaptureError> {
    /* allow cancelling capture request */
    let mut capture = tokio::select! {
        r = InputCapture::new(backend) => r?,
        _ = server.cancelled() => return Ok(()),
    };
    server.set_capture_status(Status::Enabled);

    let clients = server.active_clients();
    let clients = clients
        .iter()
        .copied()
        .map(|handle| (handle, server.get_pos(handle).expect("no such client")));

    /* create barriers for active clients */
    for (handle, pos) in clients {
        capture.create(handle, to_capture_pos(pos)).await?;
    }

    loop {
        tokio::select! {
            event = capture.next() => match event {
                Some(event) => handle_capture_event(server, &mut capture, sender_tx, event?).await?,
                None => return Ok(()),
            },
            e = rx.recv() => {
                match e {
                    Some(e) => match e {
                        CaptureRequest::Release => capture.release().await?,
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
    conn: &mut LanMouseConnection,
    event: (CaptureHandle, CaptureEvent),
) -> Result<(), CaptureError> {
    let (handle, event) = event;
    log::trace!("({handle}): {event:?}");

    if server.should_release.borrow_mut().take().is_some() {
        return capture.release().await;
    }

    if event == CaptureEvent::Begin {
        spawn_hook_command(server, handle);
    }

    let event = match event {
        CaptureEvent::Begin => ProtoEvent::Enter(lan_mouse_proto::Position::Left),
        CaptureEvent::Input(e) => ProtoEvent::Input(e),
    };

    conn.send(event, handle).await;
    Ok(())
}

fn to_capture_pos(pos: lan_mouse_ipc::Position) -> input_capture::Position {
    match pos {
        lan_mouse_ipc::Position::Left => input_capture::Position::Left,
        lan_mouse_ipc::Position::Right => input_capture::Position::Right,
        lan_mouse_ipc::Position::Top => input_capture::Position::Top,
        lan_mouse_ipc::Position::Bottom => input_capture::Position::Bottom,
    }
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
