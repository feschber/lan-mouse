use anyhow::{anyhow, Result};
use std::net::SocketAddr;

use tokio::{
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};

use crate::{client::ClientHandle, config::EmulationBackend, server::State};
use input_emulation::{
    self,
    error::{EmulationCreationError, EmulationError},
    EmulationHandle, InputEmulation,
};
use input_event::{Event, KeyboardEvent};

use super::{CaptureEvent, Server};

#[derive(Clone, Debug)]
pub enum EmulationEvent {
    /// create a new client
    Create(EmulationHandle),
    /// destroy a client
    Destroy(EmulationHandle),
    /// input emulation must release keys for client
    ReleaseKeys(ClientHandle),
    /// termination signal
    Terminate,
}

pub fn new(
    backend: Option<EmulationBackend>,
    server: Server,
    mut udp_rx: Receiver<Result<(Event, SocketAddr)>>,
    sender_tx: Sender<(Event, SocketAddr)>,
    capture_tx: Sender<CaptureEvent>,
    timer_tx: Sender<()>,
) -> Result<(JoinHandle<Result<()>>, Sender<EmulationEvent>), EmulationCreationError> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let emulate_task = tokio::task::spawn_local(async move {
        let backend = backend.map(|b| b.into());
        let mut emulate = input_emulation::create(backend).await?;
        let mut last_ignored = None;

        loop {
            tokio::select! {
                udp_event = udp_rx.recv() => {
                    let udp_event = udp_event.ok_or(anyhow!("receiver closed"))??;
                    handle_udp_rx(&server, &capture_tx, &mut emulate, &sender_tx, &mut last_ignored, udp_event, &timer_tx).await?;
                }
                emulate_event = rx.recv() => {
                    match emulate_event {
                        Some(e) => match e {
                            EmulationEvent::Create(h) => emulate.create(h).await,
                            EmulationEvent::Destroy(h) => emulate.destroy(h).await,
                            EmulationEvent::ReleaseKeys(c) => release_keys(&server, &mut emulate, c).await?,
                            EmulationEvent::Terminate => break,
                        },
                        None => break,
                    }
                }
            }
        }

        // release potentially still pressed keys
        let clients = server
            .client_manager
            .borrow()
            .get_client_states()
            .map(|(h, _)| h)
            .collect::<Vec<_>>();
        for client in clients {
            release_keys(&server, &mut emulate, client).await?;
        }

        anyhow::Ok(())
    });
    Ok((emulate_task, tx))
}

async fn handle_udp_rx(
    server: &Server,
    capture_tx: &Sender<CaptureEvent>,
    emulate: &mut Box<dyn InputEmulation>,
    sender_tx: &Sender<(Event, SocketAddr)>,
    last_ignored: &mut Option<SocketAddr>,
    event: (Event, SocketAddr),
    timer_tx: &Sender<()>,
) -> Result<(), EmulationError> {
    let (event, addr) = event;

    // get handle for addr
    let handle = match server.client_manager.borrow().get_client(addr) {
        Some(a) => a,
        None => {
            if last_ignored.is_none() || last_ignored.is_some() && last_ignored.unwrap() != addr {
                log::warn!("ignoring events from client {addr}");
                last_ignored.replace(addr);
            }
            return Ok(());
        }
    };

    // next event can be logged as ignored again
    last_ignored.take();

    log::trace!("{:20} <-<-<-<------ {addr} ({handle})", event.to_string());
    {
        let mut client_manager = server.client_manager.borrow_mut();
        let client_state = match client_manager.get_mut(handle) {
            Some((_, s)) => s,
            None => {
                log::error!("unknown handle");
                return Ok(());
            }
        };

        // reset ttl for client and
        client_state.alive = true;
        // set addr as new default for this client
        client_state.active_addr = Some(addr);
    }

    match (event, addr) {
        (Event::Pong(), _) => { /* ignore pong events */ }
        (Event::Ping(), addr) => {
            let _ = sender_tx.send((Event::Pong(), addr)).await;
        }
        (Event::Disconnect(), _) => {
            release_keys(server, emulate, handle).await?;
        }
        (event, addr) => {
            // tell clients that we are ready to receive events
            if let Event::Enter() = event {
                let _ = sender_tx.send((Event::Leave(), addr)).await;
            }

            match server.state.get() {
                State::Sending => {
                    if let Event::Leave() = event {
                        // ignore additional leave events that may
                        // have been sent for redundancy
                    } else {
                        // upon receiving any event, we go back to receiving mode
                        server.state.replace(State::Receiving);
                        let _ = capture_tx.send(CaptureEvent::Release).await;
                        log::trace!("STATE ===> Receiving");
                    }
                }
                State::Receiving => {
                    let mut ignore_event = false;
                    if let Event::Keyboard(KeyboardEvent::Key {
                        time: _,
                        key,
                        state,
                    }) = event
                    {
                        let mut client_manager = server.client_manager.borrow_mut();
                        let client_state = if let Some((_, s)) = client_manager.get_mut(handle) {
                            s
                        } else {
                            log::error!("unknown handle");
                            return Ok(());
                        };
                        if state == 0 {
                            // ignore release event if key not pressed
                            ignore_event = !client_state.pressed_keys.remove(&key);
                        } else {
                            // ignore press event if key not released
                            ignore_event = !client_state.pressed_keys.insert(key);
                            let _ = timer_tx.try_send(());
                        }
                    }
                    // ignore double press / release events to
                    // workaround buggy rdp backend.
                    if !ignore_event {
                        // consume event
                        emulate.consume(event, handle).await?;
                        log::trace!("{event} => emulate");
                    }
                }
                State::AwaitingLeave => {
                    // we just entered the deadzone of a client, so
                    // we need to ignore events that may still
                    // be on the way until a leave event occurs
                    // telling us the client registered the enter
                    if let Event::Leave() = event {
                        server.state.replace(State::Sending);
                        log::trace!("STATE ===> Sending");
                    }

                    // entering a client that is waiting for a leave
                    // event should still be possible
                    if let Event::Enter() = event {
                        server.state.replace(State::Receiving);
                        let _ = capture_tx.send(CaptureEvent::Release).await;
                        log::trace!("STATE ===> Receiving");
                    }
                }
            }
        }
    }
    Ok(())
}

async fn release_keys(
    server: &Server,
    emulate: &mut Box<dyn InputEmulation>,
    client: ClientHandle,
) -> Result<(), EmulationError> {
    let keys = server
        .client_manager
        .borrow_mut()
        .get_mut(client)
        .iter_mut()
        .flat_map(|(_, s)| s.pressed_keys.drain())
        .collect::<Vec<_>>();

    for key in keys {
        let event = Event::Keyboard(KeyboardEvent::Key {
            time: 0,
            key,
            state: 0,
        });
        emulate.consume(event, client).await?;
        if let Ok(key) = input_event::scancode::Linux::try_from(key) {
            log::warn!("releasing stuck key: {key:?}");
        }
    }

    let event = Event::Keyboard(KeyboardEvent::Modifiers {
        mods_depressed: 0,
        mods_latched: 0,
        mods_locked: 0,
        group: 0,
    });
    emulate.consume(event, client).await?;
    Ok(())
}
