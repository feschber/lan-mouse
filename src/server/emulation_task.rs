use local_channel::mpsc::{Receiver, Sender};
use std::net::SocketAddr;

use lan_mouse_proto::ProtoEvent;
use tokio::task::JoinHandle;

use lan_mouse_ipc::ClientHandle;

use crate::{client::ClientManager, server::State};
use input_emulation::{self, EmulationError, EmulationHandle, InputEmulation, InputEmulationError};
use lan_mouse_ipc::Status;

use super::{network_task::NetworkError, Server};

#[derive(Clone, Debug)]
pub(crate) enum EmulationRequest {
    /// create a new client
    Create(EmulationHandle),
    /// destroy a client
    Destroy(EmulationHandle),
    /// input emulation must release keys for client
    ReleaseKeys(ClientHandle),
}

pub(crate) fn new(
    server: Server,
    emulation_rx: Receiver<EmulationRequest>,
    udp_rx: Receiver<Result<(ProtoEvent, SocketAddr), NetworkError>>,
    sender_tx: Sender<(ProtoEvent, SocketAddr)>,
) -> JoinHandle<()> {
    let emulation_task = emulation_task(server, emulation_rx, udp_rx, sender_tx);
    tokio::task::spawn_local(emulation_task)
}

async fn emulation_task(
    server: Server,
    mut rx: Receiver<EmulationRequest>,
    mut udp_rx: Receiver<Result<(ProtoEvent, SocketAddr), NetworkError>>,
    sender_tx: Sender<(ProtoEvent, SocketAddr)>,
) {
    loop {
        if let Err(e) = do_emulation(&server, &mut rx, &mut udp_rx, &sender_tx).await {
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
    rx: &mut Receiver<EmulationRequest>,
    udp_rx: &mut Receiver<Result<(ProtoEvent, SocketAddr), NetworkError>>,
    sender_tx: &Sender<(ProtoEvent, SocketAddr)>,
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

    let res = do_emulation_session(server, &mut emulation, rx, udp_rx, sender_tx).await;
    emulation.terminate().await; // manual drop
    res
}

async fn do_emulation_session(
    server: &Server,
    emulation: &mut InputEmulation,
    rx: &mut Receiver<EmulationRequest>,
    udp_rx: &mut Receiver<Result<(ProtoEvent, SocketAddr), NetworkError>>,
    sender_tx: &Sender<(ProtoEvent, SocketAddr)>,
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
                handle_incoming_event(server, emulation, sender_tx, &mut last_ignored, udp_event).await?;
            }
            emulate_event = rx.recv() => {
                match emulate_event.expect("channel closed") {
                    EmulationRequest::Create(h) => { let _ = emulation.create(h).await; },
                    EmulationRequest::Destroy(h) => emulation.destroy(h).await,
                    EmulationRequest::ReleaseKeys(c) => emulation.release_keys(c).await?,
                }
            }
            _ = server.notifies.cancel.cancelled() => break Ok(()),
        }
    }
}

async fn handle_incoming_event(
    server: &Server,
    emulate: &mut InputEmulation,
    sender_tx: &Sender<(ProtoEvent, SocketAddr)>,
    last_ignored: &mut Option<SocketAddr>,
    event: (ProtoEvent, SocketAddr),
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
        (ProtoEvent::Pong, _) => { /* ignore pong events */ }
        (ProtoEvent::Ping, addr) => {
            let _ = sender_tx.send((ProtoEvent::Pong, addr));
        }
        (ProtoEvent::Leave(_), _) => emulate.release_keys(handle).await?,
        (ProtoEvent::Ack(_), _) => server.set_state(State::Sending),
        (ProtoEvent::Enter(_), _) => {
            server.set_state(State::Receiving);
            sender_tx
                .send((ProtoEvent::Ack(0), addr))
                .expect("no channel")
        }
        (ProtoEvent::Input(e), _) => {
            if let State::Receiving = server.get_state() {
                log::trace!("{event} => emulate");
                emulate.consume(e, handle).await?;
                let has_pressed_keys = emulate.has_pressed_keys(handle);
                server.update_pressed_keys(handle, has_pressed_keys);
                if has_pressed_keys {
                    server.restart_ping_timer();
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
