use std::{net::SocketAddr, sync::Arc};

use thiserror::Error;
use tokio::{
    sync::{
        mpsc::{Receiver, Sender},
        Notify,
    },
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    client::{ClientHandle, ClientManager},
    config::EmulationBackend,
    frontend::{FrontendEvent, Status},
    server::State,
};
use input_emulation::{
    self,
    error::{EmulationCreationError, EmulationError},
    EmulationHandle, InputEmulation,
};
use input_event::{Event, KeyboardEvent};

use super::{network_task::NetworkError, CaptureEvent, Server};

#[derive(Clone, Debug)]
pub enum EmulationEvent {
    /// create a new client
    Create(EmulationHandle),
    /// destroy a client
    Destroy(EmulationHandle),
    /// input emulation must release keys for client
    ReleaseKeys(ClientHandle),
}

pub fn new(
    backend: Option<EmulationBackend>,
    server: Server,
    udp_rx: Receiver<Result<(Event, SocketAddr), NetworkError>>,
    sender_tx: Sender<(Event, SocketAddr)>,
    capture_tx: Sender<CaptureEvent>,
    frontend_tx: Sender<FrontendEvent>,
    timer_notify: Arc<Notify>,
    cancellation_token: CancellationToken,
    notify_emulation: Arc<Notify>,
) -> (JoinHandle<()>, Sender<EmulationEvent>) {
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    let emulation_task = emulation_task(
        backend,
        rx,
        server,
        udp_rx,
        sender_tx,
        capture_tx,
        frontend_tx,
        timer_notify,
        cancellation_token,
        notify_emulation,
    );
    let emulate_task = tokio::task::spawn_local(emulation_task);
    (emulate_task, tx)
}

#[derive(Debug, Error)]
pub enum LanMouseEmulationError {
    #[error("error creating input-emulation: `{0}`")]
    Create(#[from] EmulationCreationError),
    #[error("error emulating input: `{0}`")]
    Emulate(#[from] EmulationError),
}

async fn emulation_task(
    backend: Option<EmulationBackend>,
    mut rx: Receiver<EmulationEvent>,
    server: Server,
    mut udp_rx: Receiver<Result<(Event, SocketAddr), NetworkError>>,
    sender_tx: Sender<(Event, SocketAddr)>,
    capture_tx: Sender<CaptureEvent>,
    frontend_tx: Sender<FrontendEvent>,
    timer_notify: Arc<Notify>,
    cancellation_token: CancellationToken,
    notify_emulation: Arc<Notify>,
) {
    loop {
        match do_emulation(
            backend,
            &mut rx,
            &server,
            &mut udp_rx,
            &sender_tx,
            &capture_tx,
            &frontend_tx,
            &timer_notify,
            &cancellation_token,
        )
        .await
        {
            Ok(()) => {}
            Err(e) => {
                log::warn!("input emulation exited: {e}");
            }
        }
        let _ = frontend_tx
            .send(FrontendEvent::EmulationStatus(Status::Disabled))
            .await;
        if cancellation_token.is_cancelled() {
            break;
        }
        log::info!("waiting for user to request input emulation ...");
        notify_emulation.notified().await;
        log::info!("... done");
    }
}

async fn do_emulation(
    backend: Option<EmulationBackend>,
    rx: &mut Receiver<EmulationEvent>,
    server: &Server,
    udp_rx: &mut Receiver<Result<(Event, SocketAddr), NetworkError>>,
    sender_tx: &Sender<(Event, SocketAddr)>,
    capture_tx: &Sender<CaptureEvent>,
    frontend_tx: &Sender<FrontendEvent>,
    timer_notify: &Notify,
    cancellation_token: &CancellationToken,
) -> Result<(), LanMouseEmulationError> {
    let backend = backend.map(|b| b.into());
    log::info!("creating input emulation...");
    let mut emulation = input_emulation::create(backend).await?;
    let _ = frontend_tx
        .send(FrontendEvent::EmulationStatus(Status::Enabled))
        .await;

    let res = do_emulation_session(
        &mut emulation,
        rx,
        server,
        udp_rx,
        sender_tx,
        capture_tx,
        timer_notify,
        cancellation_token,
    )
    .await;
    emulation.terminate().await;
    res?;

    // FIXME DUPLICATES
    // add clients
    // let clients = server
    //     .client_manager
    //     .borrow()
    //     .get_client_states()
    //     .map(|(h, _)| h)
    //     .collect::<Vec<_>>();
    // for handle in clients {
    //     emulation.create(handle).await;
    // }

    // release potentially still pressed keys
    release_all_keys(server, &mut emulation).await?;

    Ok(())
}

async fn do_emulation_session(
    emulation: &mut Box<dyn InputEmulation>,
    rx: &mut Receiver<EmulationEvent>,
    server: &Server,
    udp_rx: &mut Receiver<Result<(Event, SocketAddr), NetworkError>>,
    sender_tx: &Sender<(Event, SocketAddr)>,
    capture_tx: &Sender<CaptureEvent>,
    timer_notify: &Notify,
    cancellation_token: &CancellationToken,
) -> Result<(), LanMouseEmulationError> {
    let mut last_ignored = None;

    loop {
        tokio::select! {
            udp_event = udp_rx.recv() => {
                let udp_event = match udp_event.expect("channel closed") {
                    Ok(e) => e,
                    Err(e) => {
                        log::warn!("network error: {e}");
                        continue;
                    }
                };
                handle_udp_rx(&server, &capture_tx, emulation, &sender_tx, &mut last_ignored, udp_event, &timer_notify).await?;
            }
            emulate_event = rx.recv() => {
                match emulate_event.expect("channel closed") {
                    EmulationEvent::Create(h) => emulation.create(h).await,
                    EmulationEvent::Destroy(h) => emulation.destroy(h).await,
                    EmulationEvent::ReleaseKeys(c) => release_keys(&server, emulation, c).await?,
                }
            }
            _ = cancellation_token.cancelled() => break Ok(()),
        }
    }
}

async fn handle_udp_rx(
    server: &Server,
    capture_tx: &Sender<CaptureEvent>,
    emulate: &mut Box<dyn InputEmulation>,
    sender_tx: &Sender<(Event, SocketAddr)>,
    last_ignored: &mut Option<SocketAddr>,
    event: (Event, SocketAddr),
    timer_notify: &Notify,
) -> Result<(), EmulationError> {
    let (event, addr) = event;

    log::trace!("{:20} <-<-<-<------ {addr}", event.to_string());

    // get client handle for addr
    let Some(handle) =
        activate_client_if_exists(&mut server.client_manager.borrow_mut(), addr, last_ignored)
    else {
        return Ok(());
    };

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
                    let ignore_event =
                        if let Event::Keyboard(KeyboardEvent::Key { key, state, .. }) = event {
                            let (ignore_event, restart_timer) = update_client_keys(
                                &mut server.client_manager.borrow_mut(),
                                handle,
                                key,
                                state,
                            );
                            // restart timer if necessary
                            if restart_timer {
                                timer_notify.notify_one();
                            }
                            ignore_event
                        } else {
                            false
                        };
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

async fn release_all_keys(
    server: &Server,
    emulation: &mut Box<dyn InputEmulation>,
) -> Result<(), EmulationError> {
    let clients = server
        .client_manager
        .borrow()
        .get_client_states()
        .map(|(h, _)| h)
        .collect::<Vec<_>>();
    for client in clients {
        release_keys(server, emulation, client).await?;
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

fn activate_client_if_exists(
    client_manager: &mut ClientManager,
    addr: SocketAddr,
    last_ignored: &mut Option<SocketAddr>,
) -> Option<ClientHandle> {
    let Some(handle) = client_manager.get_client(addr) else {
        // log ignored if it is the first event from the client in a series
        if last_ignored.is_none() || last_ignored.is_some() && last_ignored.unwrap() != addr {
            log::warn!("ignoring events from client {addr}");
            last_ignored.replace(addr);
        }
        return None;
    };
    // next event can be logged as ignored again
    last_ignored.take();

    let (_, client_state) = client_manager.get_mut(handle)?;

    // reset ttl for client
    client_state.alive = true;
    // set addr as new default for this client
    client_state.active_addr = Some(addr);
    Some(handle)
}

fn update_client_keys(
    client_manager: &mut ClientManager,
    handle: ClientHandle,
    key: u32,
    state: u8,
) -> (bool, bool) {
    let Some(client_state) = client_manager.get_mut(handle).map(|(_, s)| s) else {
        return (true, false);
    };

    // ignore double press / release events
    let ignore_event = if state == 0 {
        // ignore release event if key not pressed
        !client_state.pressed_keys.remove(&key)
    } else {
        // ignore press event if key not released
        !client_state.pressed_keys.insert(key)
    };
    let restart_timer = !client_state.pressed_keys.is_empty();
    (ignore_event, restart_timer)
}
