use std::net::SocketAddr;

use tokio::{
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};

use crate::{
    client::{ClientHandle, ClientManager},
    frontend::Status,
    server::State,
};
use input_emulation::{self, EmulationError, EmulationHandle, InputEmulation, InputEmulationError};
use input_event::Event;

use super::{network_task::NetworkError, CaptureEvent, Server};

#[derive(Clone, Debug)]
pub(crate) enum EmulationEvent {
    /// create a new client
    Create(EmulationHandle),
    /// destroy a client
    Destroy(EmulationHandle),
    /// input emulation must release keys for client
    ReleaseKeys(ClientHandle),
}

pub(crate) fn new(
    server: Server,
    emulation_rx: Receiver<EmulationEvent>,
    udp_rx: Receiver<Result<(Event, SocketAddr), NetworkError>>,
    sender_tx: Sender<(Event, SocketAddr)>,
    capture_tx: Sender<CaptureEvent>,
) -> JoinHandle<()> {
    let emulation_task = emulation_task(server, emulation_rx, udp_rx, sender_tx, capture_tx);
    tokio::task::spawn_local(emulation_task)
}

async fn emulation_task(
    server: Server,
    mut rx: Receiver<EmulationEvent>,
    mut udp_rx: Receiver<Result<(Event, SocketAddr), NetworkError>>,
    sender_tx: Sender<(Event, SocketAddr)>,
    capture_tx: Sender<CaptureEvent>,
) {
    loop {
        if let Err(e) = do_emulation(&server, &mut rx, &mut udp_rx, &sender_tx, &capture_tx).await {
            log::warn!("input emulation exited: {e}");
        }
        server.set_emulation_status(Status::Disabled);
        if server.is_cancelled() {
            break;
        }

        // allow cancellation
        loop {
            tokio::select! {
                _ = rx.recv() => continue, /* need to ignore requests here! */
                _ = server.emulation_notified() => break,
                _ = server.cancelled() => return,
            }
        }
    }
}

async fn do_emulation(
    server: &Server,
    rx: &mut Receiver<EmulationEvent>,
    udp_rx: &mut Receiver<Result<(Event, SocketAddr), NetworkError>>,
    sender_tx: &Sender<(Event, SocketAddr)>,
    capture_tx: &Sender<CaptureEvent>,
) -> Result<(), InputEmulationError> {
    let backend = server.config.emulation_backend.map(|b| b.into());
    log::info!("creating input emulation...");
    let mut emulation = tokio::select! {
        r = InputEmulation::new(backend) => r?,
        _ = server.cancelled() => return Ok(()),
    };

    server.set_emulation_status(Status::Enabled);

    // add clients
    for handle in server.active_clients() {
        emulation.create(handle).await;
    }

    let res = do_emulation_session(server, &mut emulation, rx, udp_rx, sender_tx, capture_tx).await;

    // manual drop
    emulation.terminate().await;

    Ok(res?)
}

async fn do_emulation_session(
    server: &Server,
    emulation: &mut InputEmulation,
    rx: &mut Receiver<EmulationEvent>,
    udp_rx: &mut Receiver<Result<(Event, SocketAddr), NetworkError>>,
    sender_tx: &Sender<(Event, SocketAddr)>,
    capture_tx: &Sender<CaptureEvent>,
) -> Result<(), InputEmulationError> {
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
                handle_udp_rx(server, capture_tx, emulation, sender_tx, &mut last_ignored, udp_event).await?;
            }
            emulate_event = rx.recv() => {
                match emulate_event.expect("channel closed") {
                    EmulationEvent::Create(h) => { let _ = emulation.create(h).await; },
                    EmulationEvent::Destroy(h) => emulation.destroy(h).await,
                    EmulationEvent::ReleaseKeys(c) => emulation.release_keys(c).await?,
                }
            }
            _ = server.notifies.cancel.cancelled() => break Ok(()),
        }
    }
}

async fn handle_udp_rx(
    server: &Server,
    capture_tx: &Sender<CaptureEvent>,
    emulate: &mut InputEmulation,
    sender_tx: &Sender<(Event, SocketAddr)>,
    last_ignored: &mut Option<SocketAddr>,
    event: (Event, SocketAddr),
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
        (Event::Disconnect(), _) => emulate.release_keys(handle).await?,
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
                    log::trace!("{event} => emulate");
                    emulate.consume(event, handle).await?;
                    if emulate.has_pressed_keys(handle) {
                        server.update_pressed_keys(handle, true);
                        server.restart_ping_timer();
                    }
                    server.update_pressed_keys(handle, false);
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
