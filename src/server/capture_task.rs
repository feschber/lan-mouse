use anyhow::{anyhow, Result};
use futures::StreamExt;
use std::{collections::HashSet, net::SocketAddr, sync::Arc};
use tokio_util::sync::CancellationToken;

use tokio::{
    process::Command,
    sync::{mpsc::Sender, Notify},
    task::JoinHandle,
};

use input_capture::{self, error::CaptureCreationError, CaptureHandle, InputCapture, Position};

use input_event::{scancode, Event, KeyboardEvent};

use crate::{client::ClientHandle, config::CaptureBackend, server::State};

use super::Server;

#[derive(Clone, Copy, Debug)]
pub enum CaptureEvent {
    /// capture must release the mouse
    Release,
    /// add a capture client
    Create(CaptureHandle, Position),
    /// destory a capture client
    Destroy(CaptureHandle),
    /// restart input capture
    Restart,
}

pub fn new(
    backend: Option<CaptureBackend>,
    server: Server,
    sender_tx: Sender<(Event, SocketAddr)>,
    timer_notify: Arc<Notify>,
    release_bind: Vec<scancode::Linux>,
    cancellation_token: CancellationToken,
) -> Result<(JoinHandle<Result<()>>, Sender<CaptureEvent>), CaptureCreationError> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let backend = backend.map(|b| b.into());
    let task = tokio::task::spawn_local(async move {
        let mut capture = input_capture::create(backend).await?;
        let mut pressed_keys = HashSet::new();
        loop {
            tokio::select! {
                event = capture.next() => {
                    match event {
                        Some(Ok(event)) => handle_capture_event(&server, &mut capture, &sender_tx, &timer_notify, event, &mut pressed_keys, &release_bind).await?,
                        Some(Err(e)) => return Err(anyhow!("input capture: {e:?}")),
                        None => return Err(anyhow!("input capture terminated")),
                    }
                }
                e = rx.recv() => {
                    log::debug!("input capture notify rx: {e:?}");
                    match e {
                        Some(e) => match e {
                            CaptureEvent::Release => {
                                capture.release().await?;
                                server.state.replace(State::Receiving);
                            }
                            CaptureEvent::Create(h, p) => capture.create(h, p).await?,
                            CaptureEvent::Destroy(h) => capture.destroy(h).await?,
                            CaptureEvent::Restart => {
                                let clients = server.client_manager.borrow().get_client_states().map(|(h, (c,_))| (h, c.pos)).collect::<Vec<_>>();
                                capture.terminate().await?;
                                capture = input_capture::create(backend).await?;
                                for (handle, pos) in clients {
                                    capture.create(handle, pos.into()).await?;
                                }
                            }
                        },
                        None => break,
                    }
                }
                _ = cancellation_token.cancelled() => break,
            }
        }
        anyhow::Ok(())
    });
    Ok((task, tx))
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
    timer_notify: &Notify,
    event: (CaptureHandle, Event),
    pressed_keys: &mut HashSet<scancode::Linux>,
    release_bind: &[scancode::Linux],
) -> Result<()> {
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

    let (addr, enter, start_timer) = {
        let mut enter = false;
        let mut start_timer = false;

        // get client state for handle
        let mut client_manager = server.client_manager.borrow_mut();
        let client_state = match client_manager.get_mut(handle) {
            Some((_, s)) => s,
            None => {
                // should not happen
                log::warn!("unknown client!");
                capture.release().await?;
                server.state.replace(State::Receiving);
                log::trace!("STATE ===> Receiving");
                return Ok(());
            }
        };

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

        (client_state.active_addr, enter, start_timer)
    };
    if start_timer {
        timer_notify.notify_waiters();
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
