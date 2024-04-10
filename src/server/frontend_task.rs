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

use crate::{
    client::{ClientEvent, ClientHandle, Position},
    frontend::{self, FrontendEvent, FrontendListener, FrontendNotify},
};

use super::{
    capture_task::CaptureEvent, emulation_task::EmulationEvent, resolver_task::DnsRequest, Server,
};

pub(crate) fn new(
    mut frontend: FrontendListener,
    mut notify_rx: Receiver<FrontendNotify>,
    server: Server,
    capture: Sender<CaptureEvent>,
    emulate: Sender<EmulationEvent>,
    resolve_ch: Sender<DnsRequest>,
    port_tx: Sender<u16>,
) -> (JoinHandle<Result<()>>, Sender<FrontendEvent>) {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(32);
    let event_tx_clone = event_tx.clone();
    let frontend_task = tokio::task::spawn_local(async move {
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
                    handle_frontend_stream(&event_tx_clone, stream).await;
                }
                event = event_rx.recv() => {
                    let frontend_event = event.ok_or(anyhow!("frontend channel closed"))?;
                    if handle_frontend_event(&server, &capture, &emulate, &resolve_ch, &mut frontend, &port_tx, frontend_event).await {
                        break;
                    }
                }
                notify = notify_rx.recv() => {
                    let notify = notify.ok_or(anyhow!("frontend notify closed"))?;
                    let _ = frontend.notify_all(notify).await;
                }
            }
        }
        anyhow::Ok(())
    });
    (frontend_task, event_tx)
}

async fn handle_frontend_stream(
    frontend_tx: &Sender<FrontendEvent>,
    #[cfg(unix)] mut stream: ReadHalf<UnixStream>,
    #[cfg(windows)] mut stream: ReadHalf<TcpStream>,
) {
    use std::io;

    let tx = frontend_tx.clone();
    tokio::task::spawn_local(async move {
        let _ = tx.send(FrontendEvent::Enumerate()).await;
        loop {
            let event = frontend::read_event(&mut stream).await;
            match event {
                Ok(event) => {
                    let _ = tx.send(event).await;
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
    });
}

async fn handle_frontend_event(
    server: &Server,
    capture_tx: &Sender<CaptureEvent>,
    emulate_tx: &Sender<EmulationEvent>,
    resolve_tx: &Sender<DnsRequest>,
    frontend: &mut FrontendListener,
    port_tx: &Sender<u16>,
    event: FrontendEvent,
) -> bool {
    log::debug!("frontend: {event:?}");
    match event {
        FrontendEvent::AddClient(hostname, port, pos) => {
            add_client(
                server,
                frontend,
                resolve_tx,
                hostname,
                HashSet::new(),
                port,
                pos,
            )
            .await;
        }
        FrontendEvent::ActivateClient(handle, active) => {
            if active {
                activate_client(server, frontend, capture_tx, emulate_tx, handle).await;
            } else {
                deactivate_client(server, frontend, capture_tx, emulate_tx, handle).await;
            }
        }
        FrontendEvent::ChangePort(port) => {
            let _ = port_tx.send(port).await;
        }
        FrontendEvent::DelClient(handle) => {
            remove_client(server, frontend, capture_tx, emulate_tx, handle).await;
        }
        FrontendEvent::Enumerate() => {
            let clients = server
                .client_manager
                .borrow()
                .get_client_states()
                .map(|s| (s.client.clone(), s.active))
                .collect();
            notify_all(frontend, FrontendNotify::Enumerate(clients)).await;
        }
        FrontendEvent::Shutdown() => {
            log::info!("terminating gracefully...");
            return true;
        }
        FrontendEvent::UpdateClient(handle, hostname, port, pos) => {
            update_client(
                server,
                frontend,
                capture_tx,
                emulate_tx,
                resolve_tx,
                (handle, hostname, port, pos),
            )
            .await;
        }
    };
    false
}

async fn notify_all(frontend: &mut FrontendListener, event: FrontendNotify) {
    if let Err(e) = frontend.notify_all(event).await {
        log::error!("error notifying frontend: {e}");
    }
}

pub async fn add_client(
    server: &Server,
    frontend: &mut FrontendListener,
    resolver_tx: &Sender<DnsRequest>,
    hostname: Option<String>,
    addr: HashSet<IpAddr>,
    port: u16,
    pos: Position,
) {
    log::info!(
        "adding client [{}]{} @ {:?}",
        pos,
        hostname.as_deref().unwrap_or(""),
        &addr
    );
    let handle =
        server
            .client_manager
            .borrow_mut()
            .add_client(hostname.clone(), addr, port, pos, false);

    log::debug!("add_client {handle}");

    if let Some(hostname) = hostname {
        let _ = resolver_tx.send(DnsRequest { hostname, handle }).await;
    }
    let client = server
        .client_manager
        .borrow()
        .get(handle)
        .unwrap()
        .client
        .clone();
    notify_all(frontend, FrontendNotify::NotifyClientCreate(client)).await;
}

pub async fn deactivate_client(
    server: &Server,
    frontend: &mut FrontendListener,
    capture: &Sender<CaptureEvent>,
    emulate: &Sender<EmulationEvent>,
    client: ClientHandle,
) {
    let (client, _) = match server.client_manager.borrow_mut().get_mut(client) {
        Some(state) => {
            state.active = false;
            (state.client.handle, state.client.pos)
        }
        None => return,
    };

    let event = ClientEvent::Destroy(client);
    let _ = capture.send(CaptureEvent::ClientEvent(event)).await;
    let _ = emulate.send(EmulationEvent::ClientEvent(event)).await;
    let event = FrontendNotify::NotifyClientActivate(client, false);
    notify_all(frontend, event).await;
}

pub async fn activate_client(
    server: &Server,
    frontend: &mut FrontendListener,
    capture: &Sender<CaptureEvent>,
    emulate: &Sender<EmulationEvent>,
    handle: ClientHandle,
) {
    /* deactivate potential other client at this position */
    let pos = match server.client_manager.borrow().get(handle) {
        Some(state) => state.client.pos,
        None => return,
    };

    let other = server.client_manager.borrow_mut().find_client(pos);
    if let Some(other) = other {
        if other != handle {
            deactivate_client(server, frontend, capture, emulate, other).await;
        }
    }

    /* activate the client */
    server
        .client_manager
        .borrow_mut()
        .get_mut(handle)
        .unwrap()
        .active = true;

    /* notify emulation, capture and frontends */
    let event = ClientEvent::Create(handle, pos);
    let _ = capture.send(CaptureEvent::ClientEvent(event)).await;
    let _ = emulate.send(EmulationEvent::ClientEvent(event)).await;
    let event = FrontendNotify::NotifyClientActivate(handle, true);
    notify_all(frontend, event).await;
}

pub async fn remove_client(
    server: &Server,
    frontend: &mut FrontendListener,
    capture: &Sender<CaptureEvent>,
    emulate: &Sender<EmulationEvent>,
    client: ClientHandle,
) {
    let Some((client, active)) = server
        .client_manager
        .borrow_mut()
        .remove_client(client)
        .map(|s| (s.client.handle, s.active))
    else {
        return;
    };

    if active {
        let destroy = ClientEvent::Destroy(client);
        let _ = capture.send(CaptureEvent::ClientEvent(destroy)).await;
        let _ = emulate.send(EmulationEvent::ClientEvent(destroy)).await;
    }

    let event = FrontendNotify::NotifyClientDelete(client);
    notify_all(frontend, event).await;
}

async fn update_client(
    server: &Server,
    frontend: &mut FrontendListener,
    capture: &Sender<CaptureEvent>,
    emulate: &Sender<EmulationEvent>,
    resolve_tx: &Sender<DnsRequest>,
    client_update: (ClientHandle, Option<String>, u16, Position),
) {
    let (handle, hostname, port, pos) = client_update;
    let mut changed = false;
    let (hostname, handle, active) = {
        // retrieve state
        let mut client_manager = server.client_manager.borrow_mut();
        let Some(state) = client_manager.get_mut(handle) else {
            return;
        };

        // update pos
        if state.client.pos != pos {
            state.client.pos = pos;
            changed = true;
        }

        // update port
        if state.client.port != port {
            state.client.port = port;
            state.active_addr = state.active_addr.map(|a| SocketAddr::new(a.ip(), port));
            changed = true;
        }

        // update hostname
        if state.client.hostname != hostname {
            state.client.ips = HashSet::new();
            state.active_addr = None;
            state.client.hostname = hostname;
            changed = true;
        }

        log::debug!("client updated: {:?}", state);
        (
            state.client.hostname.clone(),
            state.client.handle,
            state.active,
        )
    };

    // resolve dns if something changed
    if changed {
        // resolve dns
        if let Some(hostname) = hostname {
            let _ = resolve_tx.send(DnsRequest { hostname, handle }).await;
        }
    }

    // update state in event input emulator & input capture
    if changed && active {
        // update state
        let destroy = ClientEvent::Destroy(handle);
        let create = ClientEvent::Create(handle, pos);
        let _ = capture.send(CaptureEvent::ClientEvent(destroy)).await;
        let _ = emulate.send(EmulationEvent::ClientEvent(destroy)).await;
        let _ = capture.send(CaptureEvent::ClientEvent(create)).await;
        let _ = emulate.send(EmulationEvent::ClientEvent(create)).await;
    }

    let client = server
        .client_manager
        .borrow()
        .get(handle)
        .unwrap()
        .client
        .clone();
    notify_all(frontend, FrontendNotify::NotifyClientUpdate(client)).await;
}
