use anyhow::anyhow;
use log;
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    io::Result,
    net::IpAddr,
    rc::Rc,
    time::Duration,
};
use tokio::{io::ReadHalf, net::UdpSocket, signal, sync::mpsc::Sender};

#[cfg(unix)]
use tokio::net::UnixStream;

#[cfg(windows)]
use tokio::net::TcpStream;

use std::{io::ErrorKind, net::SocketAddr};

use crate::{
    client::{ClientEvent, ClientHandle, ClientManager, Position},
    config::Config,
    dns,
    event::Event,
    frontend::{self, FrontendEvent, FrontendListener, FrontendNotify},
    server::producer_task::ProducerEvent,
};
use crate::{consumer, producer};

use self::consumer_task::ConsumerEvent;

mod consumer_task;
mod producer_task;

const MAX_RESPONSE_TIME: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    /// Currently sending events to another device
    Sending,
    /// Currently receiving events from other devices
    Receiving,
    /// Entered the deadzone of another device but waiting
    /// for acknowledgement (Leave event) from the device
    AwaitingLeave,
}

#[derive(Clone)]
pub struct Server {
    active_client: Rc<Cell<Option<ClientHandle>>>,
    client_manager: Rc<RefCell<ClientManager>>,
    port: Rc<Cell<u16>>,
    state: Rc<Cell<State>>,
}

impl Server {
    pub fn new(config: &Config) -> Self {
        let active_client = Rc::new(Cell::new(None));
        let client_manager = Rc::new(RefCell::new(ClientManager::new()));
        let state = Rc::new(Cell::new(State::Receiving));
        let port = Rc::new(Cell::new(config.port));
        for config_client in config.get_clients() {
            client_manager.borrow_mut().add_client(
                config_client.hostname,
                config_client.ips,
                config_client.port,
                config_client.pos,
                config_client.active,
            );
        }
        Self {
            active_client,
            client_manager,
            port,
            state,
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        // create frontend communication adapter
        let mut frontend = match FrontendListener::new().await {
            Some(f) => f?,
            None => {
                // none means some other instance is already running
                log::info!("service already running, exiting");
                return anyhow::Ok(());
            }
        };
        let (consumer, producer) = tokio::join!(consumer::create(), producer::create());

        let (frontend_tx, mut frontend_rx) = tokio::sync::mpsc::channel(32);
        let (resolve_tx, mut resolve_rx) = tokio::sync::mpsc::channel(32);
        let (frontend_notify_tx, mut frontend_notify_rx) = tokio::sync::mpsc::channel(32);
        let (receiver_tx, receiver_rx) = tokio::sync::mpsc::channel(32);
        let (sender_tx, mut sender_rx) = tokio::sync::mpsc::channel(32);
        let (port_tx, mut port_rx) = tokio::sync::mpsc::channel(32);
        let (timer_tx, mut timer_rx) = tokio::sync::mpsc::channel(1);

        // event producer
        let (mut producer_task, producer_channel) =
            producer_task::new(producer, self.clone(), sender_tx.clone(), timer_tx.clone());

        // event consumer
        let (mut consumer_task, consumer_channel) = consumer_task::new(
            consumer,
            self.clone(),
            receiver_rx,
            sender_tx.clone(),
            producer_channel.clone(),
            timer_tx,
        );

        // frontend listener
        let server = self.clone();
        let producer_notify = producer_channel.clone();
        let consumer_notify = consumer_channel.clone();
        let frontend_ch = frontend_tx.clone();
        let resolve_ch = resolve_tx.clone();
        let mut frontend_task = tokio::task::spawn_local(async move {
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
                        server.handle_frontend_stream(&frontend_ch, stream).await;
                    }
                    event = frontend_rx.recv() => {
                        let frontend_event = event.ok_or(anyhow!("frontend channel closed"))?;
                        if server.handle_frontend_event(&producer_notify, &consumer_notify, &resolve_ch, &mut frontend, &port_tx, frontend_event).await {
                            break;
                        }
                    }
                    notify = frontend_notify_rx.recv() => {
                        let notify = notify.ok_or(anyhow!("frontend notify closed"))?;
                        let _ = frontend.notify_all(notify).await;
                    }
                }
            }
            anyhow::Ok(())
        });

        // dns resolver

        // create dns resolver
        let resolver = dns::DnsResolver::new().await?;
        let server = self.clone();
        let mut resolver_task = tokio::task::spawn_local(async move {
            loop {
                let (host, client): (String, ClientHandle) = match resolve_rx.recv().await {
                    Some(r) => r,
                    None => break,
                };
                let ips = match resolver.resolve(&host).await {
                    Ok(ips) => ips,
                    Err(e) => {
                        log::warn!("could not resolve host '{host}': {e}");
                        continue;
                    }
                };
                if let Some(state) = server.client_manager.borrow_mut().get_mut(client) {
                    let mut addrs = HashSet::from_iter(state.client.fix_ips.iter().cloned());
                    for ip in ips {
                        addrs.insert(ip);
                    }
                    state.client.ips = addrs;
                }
            }
        });

        // bind the udp socket
        let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), self.port.get());
        let mut socket = UdpSocket::bind(listen_addr).await?;
        // udp task
        let mut udp_task = tokio::task::spawn_local(async move {
            loop {
                tokio::select! {
                    event = receive_event(&socket) => {
                        let _ = receiver_tx.send(event).await;
                    }
                    event = sender_rx.recv() => {
                        let Some((event, addr)) = event else {
                            break;
                        };
                        if let Err(e) = send_event(&socket, event, addr) {
                            log::warn!("udp send failed: {e}");
                        };
                    }
                    port = port_rx.recv() => {
                        let Some(port) = port else {
                            break;
                        };

                        if socket.local_addr().unwrap().port() == port {
                            continue;
                        }

                        let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), port);
                        match UdpSocket::bind(listen_addr).await {
                            Ok(new_socket) => {
                                socket = new_socket;
                                server.port.replace(port);
                                let _ = frontend_notify_tx.send(FrontendNotify::NotifyPortChange(port, None)).await;
                            }
                            Err(e) => {
                                log::warn!("could not change port: {e}");
                                let port = socket.local_addr().unwrap().port();
                                let _ = frontend_notify_tx.send(FrontendNotify::NotifyPortChange(
                                        port,
                                        Some(format!("could not change port: {e}")),
                                    )).await;
                            }
                        }

                    }
                }
            }
        });

        // timer task
        let server = self.clone();
        let sender_ch = sender_tx.clone();
        let consumer_notify = consumer_channel.clone();
        let producer_notify = producer_channel.clone();
        let mut live_tracker = tokio::task::spawn_local(async move {
            loop {
                // wait for wake up signal
                let Some(_): Option<()> = timer_rx.recv().await else {
                    break;
                };
                loop {
                    let receiving = server.state.get() == State::Receiving;
                    let (ping_clients, ping_addrs) = {
                        let mut client_manager = server.client_manager.borrow_mut();

                        let ping_clients: Vec<ClientHandle> = if receiving {
                            // if receiving we care about clients with pressed keys
                            client_manager
                                .get_client_states_mut()
                                .filter(|s| !s.pressed_keys.is_empty())
                                .map(|s| s.client.handle)
                                .collect()
                        } else {
                            // if sending we care about the active client
                            server.active_client.get().iter().cloned().collect()
                        };

                        // get relevant socket addrs for clients
                        let ping_addrs: Vec<SocketAddr> = {
                            ping_clients
                                .iter()
                                .flat_map(|&c| client_manager.get(c))
                                .flat_map(|state| {
                                    if state.alive && state.active_addr.is_some() {
                                        vec![state.active_addr.unwrap()]
                                    } else {
                                        state
                                            .client
                                            .ips
                                            .iter()
                                            .cloned()
                                            .map(|ip| SocketAddr::new(ip, state.client.port))
                                            .collect()
                                    }
                                })
                                .collect()
                        };

                        // reset alive
                        for state in client_manager.get_client_states_mut() {
                            state.alive = false;
                        }

                        (ping_clients, ping_addrs)
                    };

                    if receiving && ping_clients.is_empty() {
                        // receiving and no client has pressed keys
                        // -> no need to keep pinging
                        break;
                    }

                    // ping clients
                    for addr in ping_addrs {
                        if sender_ch.send((Event::Ping(), addr)).await.is_err() {
                            break;
                        }
                    }

                    // give clients time to resond
                    if receiving {
                        log::debug!("waiting {MAX_RESPONSE_TIME:?} for response from client with pressed keys ...");
                    } else {
                        log::debug!("state: {:?} => waiting {MAX_RESPONSE_TIME:?} for client to respond ...", server.state.get());
                    }

                    tokio::time::sleep(MAX_RESPONSE_TIME).await;

                    // when anything is received from a client,
                    // the alive flag gets set
                    let unresponsive_clients: Vec<_> = {
                        let client_manager = server.client_manager.borrow();
                        ping_clients
                            .iter()
                            .filter_map(|&c| match client_manager.get(c) {
                                Some(state) if !state.alive => Some(c),
                                _ => None,
                            })
                            .collect()
                    };

                    // we may not be receiving anymore but we should respond
                    // to the original state and not the "new" one
                    if receiving {
                        for c in unresponsive_clients {
                            log::warn!("device not responding, releasing keys!");
                            let _ = consumer_notify.send(ConsumerEvent::ReleaseKeys(c)).await;
                        }
                    } else {
                        // release pointer if the active client has not responded
                        if !unresponsive_clients.is_empty() {
                            log::warn!("client not responding, releasing pointer!");
                            server.state.replace(State::Receiving);
                            let _ = producer_notify.send(ProducerEvent::Release).await;
                        }
                    }
                }
            }
        });

        let active = self
            .client_manager
            .borrow()
            .get_client_states()
            .filter_map(|s| {
                if s.active {
                    Some((s.client.handle, s.client.hostname.clone()))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for (handle, hostname) in active {
            frontend_tx
                .send(FrontendEvent::ActivateClient(handle, true))
                .await?;
            if let Some(hostname) = hostname {
                let _ = resolve_tx.send((hostname, handle)).await;
            }
        }

        tokio::select! {
            _ = signal::ctrl_c() => {
                log::info!("terminating service");
            }
            e = &mut producer_task => {
                if let Ok(Err(e)) = e {
                    log::error!("error in event producer: {e}");
                }
            }
            e = &mut consumer_task => {
                if let Ok(Err(e)) = e {
                    log::error!("error in event consumer: {e}");
                }
            }
            e = &mut frontend_task => {
                if let Ok(Err(e)) = e {
                    log::error!("error in frontend listener: {e}");
                }
            }
            _ = &mut resolver_task => { }
            _ = &mut udp_task => { }
            _ = &mut live_tracker => { }
        }

        let _ = consumer_channel.send(ConsumerEvent::Terminate).await;
        let _ = producer_channel.send(ProducerEvent::Terminate).await;
        let _ = frontend_tx.send(FrontendEvent::Shutdown()).await;

        if !producer_task.is_finished() {
            if let Err(e) = producer_task.await {
                log::error!("error in event producer: {e}");
            }
        }
        if !consumer_task.is_finished() {
            if let Err(e) = consumer_task.await {
                log::error!("error in event consumer: {e}");
            }
        }

        if !frontend_task.is_finished() {
            if let Err(e) = frontend_task.await {
                log::error!("error in frontend listener: {e}");
            }
        }

        resolver_task.abort();
        udp_task.abort();
        live_tracker.abort();

        Ok(())
    }

    pub async fn add_client(
        &self,
        resolver_tx: &Sender<(String, ClientHandle)>,
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
            self.client_manager
                .borrow_mut()
                .add_client(hostname.clone(), addr, port, pos, false);

        log::debug!("add_client {handle}");

        if let Some(hostname) = hostname {
            let _ = resolver_tx.send((hostname, handle)).await;
        }

        handle
    }

    pub async fn activate_client(
        &self,
        producer_notify_tx: &Sender<ProducerEvent>,
        consumer_notify_tx: &Sender<ConsumerEvent>,
        client: ClientHandle,
        active: bool,
    ) {
        let (client, pos) = match self.client_manager.borrow_mut().get_mut(client) {
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
        &self,
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

        let Some(client) = self
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
        &self,
        producer_notify_tx: &Sender<ProducerEvent>,
        consumer_notify_tx: &Sender<ConsumerEvent>,
        resolve_tx: &Sender<(String, ClientHandle)>,
        client_update: (ClientHandle, Option<String>, u16, Position),
    ) {
        let (handle, hostname, port, pos) = client_update;
        let (hostname, handle, active) = {
            // retrieve state
            let mut client_manager = self.client_manager.borrow_mut();
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
            let _ = resolve_tx.send((hostname, handle)).await;
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

    async fn handle_frontend_stream(
        &self,
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
        &self,
        producer_tx: &Sender<ProducerEvent>,
        consumer_tx: &Sender<ConsumerEvent>,
        resolve_tx: &Sender<(String, ClientHandle)>,
        frontend: &mut FrontendListener,
        port_tx: &Sender<u16>,
        event: FrontendEvent,
    ) -> bool {
        log::debug!("frontend: {event:?}");
        let response = match event {
            FrontendEvent::AddClient(hostname, port, pos) => {
                let handle = self
                    .add_client(resolve_tx, hostname, HashSet::new(), port, pos)
                    .await;

                let client = self
                    .client_manager
                    .borrow()
                    .get(handle)
                    .unwrap()
                    .client
                    .clone();
                Some(FrontendNotify::NotifyClientCreate(client))
            }
            FrontendEvent::ActivateClient(handle, active) => {
                self.activate_client(producer_tx, consumer_tx, handle, active)
                    .await;
                Some(FrontendNotify::NotifyClientActivate(handle, active))
            }
            FrontendEvent::ChangePort(port) => {
                let _ = port_tx.send(port).await;
                None
            }
            FrontendEvent::DelClient(handle) => {
                self.remove_client(producer_tx, consumer_tx, frontend, handle)
                    .await;
                Some(FrontendNotify::NotifyClientDelete(handle))
            }
            FrontendEvent::Enumerate() => {
                let clients = self
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
                self.update_client(
                    producer_tx,
                    consumer_tx,
                    resolve_tx,
                    (handle, hostname, port, pos),
                )
                .await;

                let client = self
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
}

async fn receive_event(socket: &UdpSocket) -> anyhow::Result<(Event, SocketAddr)> {
    let mut buf = vec![0u8; 22];
    let (_amt, src) = socket.recv_from(&mut buf).await?;
    Ok((Event::try_from(buf)?, src))
}

fn send_event(sock: &UdpSocket, e: Event, addr: SocketAddr) -> Result<usize> {
    log::trace!("{:20} ------>->->-> {addr}", e.to_string());
    let data: Vec<u8> = (&e).into();
    // When udp blocks, we dont want to block the event loop.
    // Dropping events is better than potentially crashing the event
    // producer.
    sock.try_send_to(&data, addr)
}
