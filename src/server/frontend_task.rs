use std::{
    collections::HashSet,
    io::ErrorKind,
    net::{IpAddr, SocketAddr},
};
#[cfg(unix)]
use tokio::net::UnixStream;

#[cfg(windows)]
use tokio::net::TcpStream;

use anyhow::{anyhow, Result};
use tokio::{
    io::ReadHalf,
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    client::{ClientHandle, Position},
    frontend::{self, FrontendEvent, FrontendListener, FrontendRequest},
};

use super::{
    capture_task::CaptureEvent, emulation_task::EmulationEvent, resolver_task::DnsRequest, Server,
};

pub(crate) fn new(
    mut frontend: FrontendListener,
    mut notify_rx: Receiver<FrontendEvent>,
    server: Server,
    capture: Sender<CaptureEvent>,
    emulate: Sender<EmulationEvent>,
    resolve_ch: Sender<DnsRequest>,
    port_tx: Sender<u16>,
    cancellation_token: CancellationToken,
) -> (JoinHandle<Result<()>>, Sender<FrontendRequest>) {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(32);
    let event_tx_clone = event_tx.clone();
    let frontend_task = tokio::task::spawn_local(async move {
        let mut join_handles = vec![];
        loop {
            tokio::select! {
                stream = frontend.accept() => {
                    let stream = match stream {
                        Ok(s) => s,
                        Err(e) => {
                            log::warn!("error accepting frontend connection: {e}");
                            continue;
                        }
                    };
                    join_handles.push(handle_frontend_stream(&event_tx_clone, stream, cancellation_token.clone()));
                }
                event = event_rx.recv() => {
                    let frontend_event = event.ok_or(anyhow!("frontend channel closed"))?;
                    if handle_frontend_event(&server, &capture, &emulate, &resolve_ch, &mut frontend, &port_tx, frontend_event).await {
                        break;
                    }
                }
                notify = notify_rx.recv() => {
                    let notify = notify.ok_or(anyhow!("frontend notify closed"))?;
                    let _ = frontend.broadcast_event(notify).await;
                }
                _ = cancellation_token.cancelled() => {
                    futures::future::join_all(join_handles).await;
                    break;
                }
            }
        }
        anyhow::Ok(())
    });
    (frontend_task, event_tx)
}

fn handle_frontend_stream(
    frontend_tx: &Sender<FrontendRequest>,
    #[cfg(unix)] stream: ReadHalf<UnixStream>,
    #[cfg(windows)] stream: ReadHalf<TcpStream>,
    cancellation_token: CancellationToken,
) -> JoinHandle<()> {
    let tx = frontend_tx.clone();
    tokio::task::spawn_local(async move {
        tokio::select! {
            _ = listen_frontend(tx, stream) => {},
            _ = cancellation_token.cancelled() => {},
        }
    })
}

async fn listen_frontend(
    tx: Sender<FrontendRequest>,
    #[cfg(unix)] mut stream: ReadHalf<UnixStream>,
    #[cfg(windows)] mut stream: ReadHalf<TcpStream>,
) {
    use std::io;
    loop {
        let request = frontend::wait_for_request(&mut stream).await;
        match request {
            Ok(request) => {
                let _ = tx.send(request).await;
            }
            Err(e) => {
                if let Some(e) = e.downcast_ref::<io::Error>() {
                    if e.kind() == ErrorKind::UnexpectedEof {
                        return;
                    }
                }
                log::error!("error reading frontend event: {e}");
                return;
            }
        }
    }
}

async fn handle_frontend_event(
    server: &Server,
    capture: &Sender<CaptureEvent>,
    emulate: &Sender<EmulationEvent>,
    resolve_tx: &Sender<DnsRequest>,
    frontend: &mut FrontendListener,
    port_tx: &Sender<u16>,
    event: FrontendRequest,
) -> bool {
    log::debug!("frontend: {event:?}");
    match event {
        FrontendRequest::EnableCapture => {
            let _ = capture.send(CaptureEvent::Restart).await;
        }
        FrontendRequest::EnableEmulation => {
            let _ = emulate.send(EmulationEvent::Restart).await;
        }
        FrontendRequest::Create => {
            let handle = add_client(server, frontend).await;
            resolve_dns(server, resolve_tx, handle).await;
        }
        FrontendRequest::Activate(handle, active) => {
            if active {
                activate_client(server, capture, emulate, handle).await;
            } else {
                deactivate_client(server, capture, emulate, handle).await;
            }
        }
        FrontendRequest::ChangePort(port) => {
            let _ = port_tx.send(port).await;
        }
        FrontendRequest::Delete(handle) => {
            remove_client(server, capture, emulate, handle).await;
            broadcast(frontend, FrontendEvent::Deleted(handle)).await;
        }
        FrontendRequest::Enumerate() => {
            let clients = server
                .client_manager
                .borrow()
                .get_client_states()
                .map(|(h, (c, s))| (h, c.clone(), s.clone()))
                .collect();
            broadcast(frontend, FrontendEvent::Enumerate(clients)).await;
        }
        FrontendRequest::GetState(handle) => {
            broadcast_client(server, frontend, handle).await;
        }
        FrontendRequest::Terminate() => {
            log::info!("terminating gracefully...");
            return true;
        }
        FrontendRequest::UpdateFixIps(handle, fix_ips) => {
            update_fix_ips(server, handle, fix_ips).await;
            resolve_dns(server, resolve_tx, handle).await;
        }
        FrontendRequest::UpdateHostname(handle, hostname) => {
            update_hostname(server, resolve_tx, handle, hostname).await;
            resolve_dns(server, resolve_tx, handle).await;
        }
        FrontendRequest::UpdatePort(handle, port) => {
            update_port(server, handle, port).await;
        }
        FrontendRequest::UpdatePosition(handle, pos) => {
            update_pos(server, handle, capture, emulate, pos).await;
        }
        FrontendRequest::ResolveDns(handle) => {
            resolve_dns(server, resolve_tx, handle).await;
        }
    };
    false
}

async fn resolve_dns(server: &Server, resolve_tx: &Sender<DnsRequest>, handle: ClientHandle) {
    let hostname = server
        .client_manager
        .borrow()
        .get(handle)
        .and_then(|(c, _)| c.hostname.clone());
    if let Some(hostname) = hostname {
        let _ = resolve_tx
            .send(DnsRequest {
                hostname: hostname.clone(),
                handle,
            })
            .await;
    }
}

async fn broadcast(frontend: &mut FrontendListener, event: FrontendEvent) {
    if let Err(e) = frontend.broadcast_event(event).await {
        log::error!("error notifying frontend: {e}");
    }
}

pub async fn add_client(server: &Server, frontend: &mut FrontendListener) -> ClientHandle {
    let handle = server.client_manager.borrow_mut().add_client();
    log::info!("added client {handle}");

    let (c, s) = server.client_manager.borrow().get(handle).unwrap().clone();
    broadcast(frontend, FrontendEvent::Created(handle, c, s)).await;
    handle
}

pub async fn deactivate_client(
    server: &Server,
    capture: &Sender<CaptureEvent>,
    emulate: &Sender<EmulationEvent>,
    handle: ClientHandle,
) {
    match server.client_manager.borrow_mut().get_mut(handle) {
        Some((_, s)) => {
            s.active = false;
        }
        None => return,
    };

    let _ = capture.send(CaptureEvent::Destroy(handle)).await;
    let _ = emulate.send(EmulationEvent::Destroy(handle)).await;
}

pub async fn activate_client(
    server: &Server,
    capture: &Sender<CaptureEvent>,
    emulate: &Sender<EmulationEvent>,
    handle: ClientHandle,
) {
    /* deactivate potential other client at this position */
    let pos = match server.client_manager.borrow().get(handle) {
        Some((client, _)) => client.pos,
        None => return,
    };

    let other = server.client_manager.borrow_mut().find_client(pos);
    if let Some(other) = other {
        if other != handle {
            deactivate_client(server, capture, emulate, other).await;
        }
    }

    /* activate the client */
    if let Some((_, s)) = server.client_manager.borrow_mut().get_mut(handle) {
        s.active = true;
    } else {
        return;
    };

    /* notify emulation, capture and frontends */
    let _ = capture.send(CaptureEvent::Create(handle, pos.into())).await;
    let _ = emulate.send(EmulationEvent::Create(handle)).await;
}

pub async fn remove_client(
    server: &Server,
    capture: &Sender<CaptureEvent>,
    emulate: &Sender<EmulationEvent>,
    handle: ClientHandle,
) {
    let Some(active) = server
        .client_manager
        .borrow_mut()
        .remove_client(handle)
        .map(|(_, s)| s.active)
    else {
        return;
    };

    if active {
        let _ = capture.send(CaptureEvent::Destroy(handle)).await;
        let _ = emulate.send(EmulationEvent::Destroy(handle)).await;
    }
}

async fn update_fix_ips(server: &Server, handle: ClientHandle, fix_ips: Vec<IpAddr>) {
    let mut client_manager = server.client_manager.borrow_mut();
    let Some((c, _)) = client_manager.get_mut(handle) else {
        return;
    };

    c.fix_ips = fix_ips;
}

async fn update_hostname(
    server: &Server,
    resolve_tx: &Sender<DnsRequest>,
    handle: ClientHandle,
    hostname: Option<String>,
) {
    let hostname = {
        let mut client_manager = server.client_manager.borrow_mut();
        let Some((c, s)) = client_manager.get_mut(handle) else {
            return;
        };

        // update hostname
        if c.hostname != hostname {
            c.hostname = hostname;
            s.ips = HashSet::from_iter(c.fix_ips.iter().cloned());
            s.active_addr = None;
            c.hostname.clone()
        } else {
            None
        }
    };

    // resolve to update ips in state
    if let Some(hostname) = hostname {
        let _ = resolve_tx.send(DnsRequest { hostname, handle }).await;
    }
}

async fn update_port(server: &Server, handle: ClientHandle, port: u16) {
    let mut client_manager = server.client_manager.borrow_mut();
    let Some((c, s)) = client_manager.get_mut(handle) else {
        return;
    };

    if c.port != port {
        c.port = port;
        s.active_addr = s.active_addr.map(|a| SocketAddr::new(a.ip(), port));
    }
}

async fn update_pos(
    server: &Server,
    handle: ClientHandle,
    capture: &Sender<CaptureEvent>,
    emulate: &Sender<EmulationEvent>,
    pos: Position,
) {
    let (changed, active) = {
        let mut client_manager = server.client_manager.borrow_mut();
        let Some((c, s)) = client_manager.get_mut(handle) else {
            return;
        };

        let changed = c.pos != pos;
        c.pos = pos;
        (changed, s.active)
    };

    // update state in event input emulator & input capture
    if changed {
        if active {
            let _ = capture.send(CaptureEvent::Destroy(handle)).await;
            let _ = emulate.send(EmulationEvent::Destroy(handle)).await;
        }
        let _ = capture.send(CaptureEvent::Create(handle, pos.into())).await;
        let _ = emulate.send(EmulationEvent::Create(handle)).await;
    }
}

async fn broadcast_client(server: &Server, frontend: &mut FrontendListener, handle: ClientHandle) {
    let client = server.client_manager.borrow().get(handle).cloned();
    if let Some((config, state)) = client {
        broadcast(frontend, FrontendEvent::State(handle, config, state)).await;
    } else {
        broadcast(frontend, FrontendEvent::NoSuchClient(handle)).await;
    }
}
