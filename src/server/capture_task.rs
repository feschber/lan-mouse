use futures::StreamExt;
use std::net::SocketAddr;

use tokio::{
    process::Command,
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};

use input_capture::{self, CaptureError, CaptureHandle, InputCapture, InputCaptureError, Position};

use input_event::Event;

use crate::{client::ClientHandle, frontend::Status, server::State};

use super::Server;

#[derive(Clone, Copy, Debug)]
pub(crate) enum CaptureEvent {
    /// capture must release the mouse
    Release,
    /// add a capture client
    Create(CaptureHandle, Position),
    /// destory a capture client
    Destroy(CaptureHandle),
}

pub(crate) fn new(
    server: Server,
    capture_rx: Receiver<CaptureEvent>,
    udp_send: Sender<(Event, SocketAddr)>,
) -> JoinHandle<()> {
    let backend = server.config.capture_backend.map(|b| b.into());
    tokio::task::spawn_local(capture_task(server, backend, udp_send, capture_rx))
}

async fn capture_task(
    server: Server,
    backend: Option<input_capture::Backend>,
    sender_tx: Sender<(Event, SocketAddr)>,
    mut notify_rx: Receiver<CaptureEvent>,
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
                _ = server.capture_notified() => break,
                _ = server.cancelled() => return,
            }
        }
    }
}

async fn do_capture(
    backend: Option<input_capture::Backend>,
    server: &Server,
    sender_tx: &Sender<(Event, SocketAddr)>,
    notify_rx: &mut Receiver<CaptureEvent>,
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
        capture.create(handle, pos.into()).await?;
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
                        CaptureEvent::Release => {
                            capture.release().await?;
                            server.state.replace(State::Receiving);
                        }
                        CaptureEvent::Create(h, p) => capture.create(h, p).await?,
                        CaptureEvent::Destroy(h) => capture.destroy(h).await?,
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
    sender_tx: &Sender<(Event, SocketAddr)>,
    event: (CaptureHandle, Event),
) -> Result<(), CaptureError> {
    let (handle, mut e) = event;
    log::trace!("({handle}) {e:?}");

    // check release bind
    if capture.keys_pressed(&server.release_bind) {
        capture.release().await?;
        server.state.replace(State::Receiving);
        e = Event::Disconnect();
    }

    let info = {
        let mut enter = false;
        let mut start_timer = false;

        // get client state for handle
        let mut client_manager = server.client_manager.borrow_mut();
        let client_state = client_manager.get_mut(handle).map(|(_, s)| s);
        if let Some(client_state) = client_state {
            // if we just entered the client we want to send additional enter events until
            // we get a leave event
            if let Event::Enter() = e {
                server.state.replace(State::AwaitingLeave);
                server.active_client.replace(Some(handle));
                log::trace!("Active client => {}", handle);
                start_timer = true;
                log::trace!("STATE ===> AwaitingLeave");
                enter = true;
            } else {
                // ignore any potential events in receiving mode
                if server.state.get() == State::Receiving && e != Event::Disconnect() {
                    return Ok(());
                }
            }
            Some((client_state.active_addr, enter, start_timer))
        } else {
            None
        }
    };

    let (addr, enter, start_timer) = match info {
        Some(i) => i,
        None => {
            // should not happen
            log::warn!("unknown client!");
            capture.release().await?;
            server.state.replace(State::Receiving);
            log::trace!("STATE ===> Receiving");
            return Ok(());
        }
    };

    if start_timer {
        server.restart_ping_timer();
    }
    if enter {
        spawn_hook_command(server, handle);
    }
    if let Some(addr) = addr {
        if enter {
            let _ = sender_tx.send((Event::Enter(), addr)).await;
        }
        let _ = sender_tx.send((e, addr)).await;
    }
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
