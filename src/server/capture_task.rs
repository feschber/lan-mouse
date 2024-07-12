use futures::StreamExt;
use std::{collections::HashSet, net::SocketAddr};
use thiserror::Error;

use tokio::{
    process::Command,
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};

use input_capture::{
    self, error::CaptureCreationError, CaptureError, CaptureHandle, InputCapture, Position,
};

use input_event::{scancode, Event, KeyboardEvent};

use crate::{
    client::ClientHandle,
    config::CaptureBackend,
    frontend::{FrontendEvent, Status},
    server::State,
};

use super::Server;

#[derive(Debug, Error)]
pub enum LanMouseCaptureError {
    #[error("error creating input-capture: `{0}`")]
    Create(#[from] CaptureCreationError),
    #[error("error while capturing input: `{0}`")]
    Capture(#[from] CaptureError),
}

#[derive(Clone, Copy, Debug)]
pub enum CaptureEvent {
    /// capture must release the mouse
    Release,
    /// add a capture client
    Create(CaptureHandle, Position),
    /// destory a capture client
    Destroy(CaptureHandle),
}

pub fn new(
    server: Server,
    backend: Option<CaptureBackend>,
    capture_rx: Receiver<CaptureEvent>,
    udp_send: Sender<(Event, SocketAddr)>,
    frontend_tx: Sender<FrontendEvent>,
    release_bind: Vec<scancode::Linux>,
) -> JoinHandle<()> {
    let backend = backend.map(|b| b.into());
    tokio::task::spawn_local(capture_task(
        server,
        backend,
        udp_send,
        capture_rx,
        frontend_tx,
        release_bind,
    ))
}

async fn capture_task(
    server: Server,
    backend: Option<input_capture::Backend>,
    sender_tx: Sender<(Event, SocketAddr)>,
    mut notify_rx: Receiver<CaptureEvent>,
    frontend_tx: Sender<FrontendEvent>,
    release_bind: Vec<scancode::Linux>,
) {
    loop {
        if let Err(e) = do_capture(
            backend,
            &server,
            &sender_tx,
            &mut notify_rx,
            &frontend_tx,
            &release_bind,
        )
        .await
        {
            log::warn!("input capture exited: {e}");
        }
        let _ = frontend_tx
            .send(FrontendEvent::CaptureStatus(Status::Disabled))
            .await;
        if server.is_cancelled() {
            break;
        }
        server.capture_notified().await;
    }
}

async fn do_capture(
    backend: Option<input_capture::Backend>,
    server: &Server,
    sender_tx: &Sender<(Event, SocketAddr)>,
    notify_rx: &mut Receiver<CaptureEvent>,
    frontend_tx: &Sender<FrontendEvent>,
    release_bind: &[scancode::Linux],
) -> Result<(), LanMouseCaptureError> {
    /* allow cancelling capture request */
    let mut capture = tokio::select! {
        r = input_capture::create(backend) => {
            r?
        },
        _ = server.cancelled() => return Ok(()),
    };

    let _ = frontend_tx
        .send(FrontendEvent::CaptureStatus(Status::Enabled))
        .await;

    // FIXME DUPLICATES
    let clients = server
        .client_manager
        .borrow()
        .get_client_states()
        .map(|(h, s)| (h, s.clone()))
        .collect::<Vec<_>>();
    log::info!("{clients:?}");
    // let clients = server
    //     .client_manager
    //     .borrow()
    //     .get_client_states()
    //     .map(|(h, (c, _))| (h, c.pos))
    //     .collect::<Vec<_>>();
    // for (handle, pos) in clients {
    //     capture.create(handle, pos.into()).await?;
    // }

    let mut pressed_keys = HashSet::new();
    loop {
        tokio::select! {
            event = capture.next() => {
                match event {
                    Some(Ok(event)) => handle_capture_event(server, &mut capture, sender_tx, event, &mut pressed_keys, release_bind).await?,
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()),
                }
            }
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

fn update_pressed_keys(pressed_keys: &mut HashSet<scancode::Linux>, key: u32, state: u8) {
    if let Ok(scancode) = scancode::Linux::try_from(key) {
        log::debug!("key: {key}, state: {state}, scancode: {scancode:?}");
        match state {
            1 => pressed_keys.insert(scancode),
            _ => pressed_keys.remove(&scancode),
        };
    }
}

async fn handle_capture_event(
    server: &Server,
    capture: &mut Box<dyn InputCapture>,
    sender_tx: &Sender<(Event, SocketAddr)>,
    event: (CaptureHandle, Event),
    pressed_keys: &mut HashSet<scancode::Linux>,
    release_bind: &[scancode::Linux],
) -> Result<(), CaptureError> {
    let (handle, mut e) = event;
    log::trace!("({handle}) {e:?}");

    if let Event::Keyboard(KeyboardEvent::Key { key, state, .. }) = e {
        update_pressed_keys(pressed_keys, key, state);
        log::debug!("{pressed_keys:?}");
        if release_bind.iter().all(|k| pressed_keys.contains(k)) {
            pressed_keys.clear();
            log::info!("releasing pointer");
            capture.release().await?;
            server.state.replace(State::Receiving);
            log::trace!("STATE ===> Receiving");
            // send an event to release all the modifiers
            e = Event::Disconnect();
        }
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
