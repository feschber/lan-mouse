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
    consumer_task::ConsumerEvent, producer_task::ProducerEvent, resolver_task::DnsRequest, Server,
};

pub(crate) fn new(
    mut frontend: FrontendListener,
    mut notify_rx: Receiver<FrontendNotify>,
    server: Server,
    producer_notify: Sender<ProducerEvent>,
    consumer_notify: Sender<ConsumerEvent>,
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
                    if handle_frontend_event(&server, &producer_notify, &consumer_notify, &resolve_ch, &mut frontend, &port_tx, frontend_event).await {
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
    producer_tx: &Sender<ProducerEvent>,
    consumer_tx: &Sender<ConsumerEvent>,
    resolve_tx: &Sender<DnsRequest>,
    frontend: &mut FrontendListener,
    port_tx: &Sender<u16>,
    event: FrontendEvent,
) -> bool {
    log::debug!("frontend: {event:?}");
    let response = match event {
        FrontendEvent::AddClient(hostname, port, pos) => {
            let handle = add_client(server, resolve_tx, hostname, HashSet::new(), port, pos).await;

            let client = server
                .client_manager
                .borrow()
                .get(handle)
                .unwrap()
                .client
                .clone();
            Some(FrontendNotify::NotifyClientCreate(client))
        }
        FrontendEvent::ActivateClient(handle, active) => {
            activate_client(server, producer_tx, consumer_tx, handle, active).await;
            Some(FrontendNotify::NotifyClientActivate(handle, active))
        }
        FrontendEvent::ChangePort(port) => {
            let _ = port_tx.send(port).await;
            None
        }
        FrontendEvent::DelClient(handle) => {
            remove_client(server, producer_tx, consumer_tx, frontend, handle).await;
            Some(FrontendNotify::NotifyClientDelete(handle))
        }
        FrontendEvent::Enumerate() => {
            let clients = server
                .client_manager
                .borrow()
                .get_client_states()
                .map(|s| (s.client.clone(), s.active))
                .collect();
            Some(FrontendNotify::Enumerate(clients))
        }
        FrontendEvent::Shutdown() => {
            log::info!("terminating gracefully...");
            return true;
        }
        FrontendEvent::UpdateClient(handle, hostname, port, pos) => {
            update_client(
                server,
                producer_tx,
                consumer_tx,
                resolve_tx,
                (handle, hostname, port, pos),
            )
            .await;

            let client = server
                .client_manager
                .borrow()
                .get(handle)
                .unwrap()
                .client
                .clone();
            Some(FrontendNotify::NotifyClientUpdate(client))
        }
    };
    let Some(response) = response else {
        return false;
    };
    if let Err(e) = frontend.notify_all(response).await {
        log::error!("error notifying frontend: {e}");
    }
    false
}

pub async fn add_client(
    server: &Server,
    resolver_tx: &Sender<DnsRequest>,
    hostname: Option<String>,
    addr: HashSet<IpAddr>,
    port: u16,
    pos: Position,
) -> ClientHandle {
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

    handle
}

pub async fn activate_client(
    server: &Server,
    producer_notify_tx: &Sender<ProducerEvent>,
    consumer_notify_tx: &Sender<ConsumerEvent>,
    client: ClientHandle,
    active: bool,
) {
    let (client, pos) = match server.client_manager.borrow_mut().get_mut(client) {
        Some(state) => {
            state.active = active;
            (state.client.handle, state.client.pos)
        }
        None => return,
    };
    if active {
        let _ = producer_notify_tx
            .send(ProducerEvent::ClientEvent(ClientEvent::Create(client, pos)))
            .await;
        let _ = consumer_notify_tx
            .send(ConsumerEvent::ClientEvent(ClientEvent::Create(client, pos)))
            .await;
    } else {
        let _ = producer_notify_tx
            .send(ProducerEvent::ClientEvent(ClientEvent::Destroy(client)))
            .await;
        let _ = consumer_notify_tx
            .send(ConsumerEvent::ClientEvent(ClientEvent::Destroy(client)))
            .await;
    }
}

pub async fn remove_client(
    server: &Server,
    producer_notify_tx: &Sender<ProducerEvent>,
    consumer_notify_tx: &Sender<ConsumerEvent>,
    frontend: &mut FrontendListener,
    client: ClientHandle,
) -> Option<ClientHandle> {
    let _ = producer_notify_tx
        .send(ProducerEvent::ClientEvent(ClientEvent::Destroy(client)))
        .await;
    let _ = consumer_notify_tx
        .send(ConsumerEvent::ClientEvent(ClientEvent::Destroy(client)))
        .await;

    let Some(client) = server
        .client_manager
        .borrow_mut()
        .remove_client(client)
        .map(|s| s.client.handle)
    else {
        return None;
    };

    let notify = FrontendNotify::NotifyClientDelete(client);
    log::debug!("{notify:?}");
    if let Err(e) = frontend.notify_all(notify).await {
        log::error!("error notifying frontend: {e}");
    }
    Some(client)
}

async fn update_client(
    server: &Server,
    producer_notify_tx: &Sender<ProducerEvent>,
    consumer_notify_tx: &Sender<ConsumerEvent>,
    resolve_tx: &Sender<DnsRequest>,
    client_update: (ClientHandle, Option<String>, u16, Position),
) {
    let (handle, hostname, port, pos) = client_update;
    let (hostname, handle, active) = {
        // retrieve state
        let mut client_manager = server.client_manager.borrow_mut();
        let Some(state) = client_manager.get_mut(handle) else {
            return;
        };

        // update pos
        state.client.pos = pos;

        // update port
        if state.client.port != port {
            state.client.port = port;
            state.active_addr = state.active_addr.map(|a| SocketAddr::new(a.ip(), port));
        }

        // update hostname
        if state.client.hostname != hostname {
            state.client.ips = HashSet::new();
            state.active_addr = None;
            state.client.hostname = hostname;
        }

        log::debug!("client updated: {:?}", state);
        (
            state.client.hostname.clone(),
            state.client.handle,
            state.active,
        )
    };

    // resolve dns
    if let Some(hostname) = hostname {
        let _ = resolve_tx.send(DnsRequest { hostname, handle }).await;
    }

    // update state in event consumer & producer
    if active {
        let _ = producer_notify_tx
            .send(ProducerEvent::ClientEvent(ClientEvent::Destroy(handle)))
            .await;
        let _ = consumer_notify_tx
            .send(ConsumerEvent::ClientEvent(ClientEvent::Destroy(handle)))
            .await;
        let _ = producer_notify_tx
            .send(ProducerEvent::ClientEvent(ClientEvent::Create(handle, pos)))
            .await;
        let _ = consumer_notify_tx
            .send(ConsumerEvent::ClientEvent(ClientEvent::Create(handle, pos)))
            .await;
    }
}
