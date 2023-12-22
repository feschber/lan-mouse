use futures::stream::StreamExt;
use log;
use std::{
    collections::HashSet,
    error::Error,
    io::Result,
    net::IpAddr,
    time::{Duration, Instant},
};
use tokio::{
    io::ReadHalf,
    net::UdpSocket,
    signal,
    sync::mpsc::{Receiver, Sender},
};

#[cfg(unix)]
use tokio::net::UnixStream;

#[cfg(windows)]
use tokio::net::TcpStream;

use std::{io::ErrorKind, net::SocketAddr};

use crate::event::{Event, KeyboardEvent};
use crate::{
    client::{ClientEvent, ClientHandle, ClientManager, Position},
    config::Config,
    consumer::EventConsumer,
    dns::{self, DnsResolver},
    frontend::{self, FrontendEvent, FrontendListener, FrontendNotify},
    producer::EventProducer,
};

#[derive(Debug, Eq, PartialEq)]
enum State {
    Sending,
    Receiving,
    AwaitingLeave,
}

pub struct Server {
    resolver: DnsResolver,
    client_manager: ClientManager,
    state: State,
    frontend: FrontendListener,
    consumer: Box<dyn EventConsumer>,
    producer: Box<dyn EventProducer>,
    socket: UdpSocket,
    frontend_rx: Receiver<FrontendEvent>,
    frontend_tx: Sender<FrontendEvent>,
    last_ignored: Option<SocketAddr>,
}

impl Server {
    pub async fn new(
        config: &Config,
        frontend: FrontendListener,
        consumer: Box<dyn EventConsumer>,
        producer: Box<dyn EventProducer>,
    ) -> anyhow::Result<Self> {
        // create dns resolver
        let resolver = dns::DnsResolver::new().await?;

        // bind the udp socket
        let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), config.port);
        let socket = UdpSocket::bind(listen_addr).await?;
        let (frontend_tx, frontend_rx) = tokio::sync::mpsc::channel(1);

        // create client manager
        let client_manager = ClientManager::new();
        let mut server = Server {
            frontend,
            consumer,
            producer,
            resolver,
            socket,
            client_manager,
            state: State::Receiving,
            frontend_rx,
            frontend_tx,
            last_ignored: None,
        };

        // add clients from config
        for (c, h, port, p) in config.get_clients().into_iter() {
            server.add_client(h, c, port, p).await;
        }

        Ok(server)
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        loop {
            log::trace!("polling...");
            tokio::select! { biased;
                // safety: cancellation safe
                res = self.producer.next() => {
                    match res {
                        Some(Ok((client, event))) => {
                            self.handle_producer_event(client,event).await;
                        },
                        Some(Err(e)) => return Err(e.into()),
                        _ => break,
                    }
                }
                // safety: cancellation safe
                udp_event = receive_event(&self.socket) => {
                    match udp_event {
                        Ok(e) => self.handle_udp_rx(e).await,
                        Err(e) => log::error!("error reading event: {e}"),
                    }
                }
                // safety: cancellation safe
                stream = self.frontend.accept() => {
                    match stream {
                        Ok(s) => self.handle_frontend_stream(s).await,
                        Err(e) => log::error!("error connecting to frontend: {e}"),
                    }
                }
                // safety: cancellation safe
                frontend_event = self.frontend_rx.recv() => {
                    if let Some(event) = frontend_event {
                        if self.handle_frontend_event(event).await {
                            break;
                        }
                    }
                }
                // safety: cancellation safe
                e = self.consumer.dispatch() => {
                    e?;
                }
                // safety: cancellation safe
                _ = signal::ctrl_c() => {
                    log::info!("terminating gracefully ...");
                    break;
                }
            }
        }

        // destroy consumer
        self.consumer.destroy().await;

        Ok(())
    }

    pub async fn add_client(
        &mut self,
        hostname: Option<String>,
        mut addr: HashSet<IpAddr>,
        port: u16,
        pos: Position,
    ) -> ClientHandle {
        let ips = if let Some(hostname) = hostname.as_ref() {
            match self.resolver.resolve(hostname.as_str()).await {
                Ok(ips) => HashSet::from_iter(ips.iter().cloned()),
                Err(e) => {
                    log::warn!("could not resolve host: {e}");
                    HashSet::new()
                }
            }
        } else {
            HashSet::new()
        };
        addr.extend(ips.iter());
        log::info!(
            "adding client [{}]{} @ {:?}",
            pos,
            hostname.as_deref().unwrap_or(""),
            &ips
        );
        let client = self
            .client_manager
            .add_client(hostname.clone(), addr, port, pos);
        log::debug!("add_client {client}");
        let notify = FrontendNotify::NotifyClientCreate(client, hostname, port, pos);
        if let Err(e) = self.frontend.notify_all(notify).await {
            log::error!("error notifying frontend: {e}");
        };
        client
    }

    pub async fn activate_client(&mut self, client: ClientHandle, active: bool) {
        if let Some(state) = self.client_manager.get_mut(client) {
            state.active = active;
            if state.active {
                self.producer
                    .notify(ClientEvent::Create(client, state.client.pos));
                self.consumer
                    .notify(ClientEvent::Create(client, state.client.pos))
                    .await;
            } else {
                self.producer.notify(ClientEvent::Destroy(client));
                self.consumer.notify(ClientEvent::Destroy(client)).await;
            }
        }
    }

    pub async fn remove_client(&mut self, client: ClientHandle) -> Option<ClientHandle> {
        self.producer.notify(ClientEvent::Destroy(client));
        self.consumer.notify(ClientEvent::Destroy(client)).await;
        if let Some(client) = self
            .client_manager
            .remove_client(client)
            .map(|s| s.client.handle)
        {
            let notify = FrontendNotify::NotifyClientDelete(client);
            log::debug!("{notify:?}");
            if let Err(e) = self.frontend.notify_all(notify).await {
                log::error!("error notifying frontend: {e}");
            }
            Some(client)
        } else {
            None
        }
    }

    pub async fn update_client(
        &mut self,
        client: ClientHandle,
        hostname: Option<String>,
        port: u16,
        pos: Position,
    ) {
        // retrieve state
        let Some(state) = self.client_manager.get_mut(client) else {
            return;
        };

        // update pos
        state.client.pos = pos;
        if state.active {
            self.producer.notify(ClientEvent::Destroy(client));
            self.consumer.notify(ClientEvent::Destroy(client)).await;
            self.producer.notify(ClientEvent::Create(client, pos));
            self.consumer.notify(ClientEvent::Create(client, pos)).await;
        }

        // update port
        if state.client.port != port {
            state.client.port = port;
            state.client.addrs = state
                .client
                .addrs
                .iter()
                .cloned()
                .map(|mut a| {
                    a.set_port(port);
                    a
                })
                .collect();
            state
                .client
                .active_addr
                .map(|a| SocketAddr::new(a.ip(), port));
        }

        // update hostname
        if state.client.hostname != hostname {
            state.client.addrs = HashSet::new();
            state.client.active_addr = None;
            state.client.hostname = hostname;
            if let Some(hostname) = state.client.hostname.as_ref() {
                match self.resolver.resolve(hostname.as_str()).await {
                    Ok(ips) => {
                        let addrs = ips.iter().map(|i| SocketAddr::new(*i, port));
                        state.client.addrs = HashSet::from_iter(addrs);
                    }
                    Err(e) => {
                        log::warn!("could not resolve host: {e}");
                    }
                }
            }
        }
        log::debug!("client updated: {:?}", state);
    }

    async fn handle_udp_rx(&mut self, event: (Event, SocketAddr)) {
        let (event, addr) = event;

        // get handle for addr
        let handle = match self.client_manager.get_client(addr) {
            Some(a) => a,
            None => {
                if self.last_ignored.is_none()
                    || self.last_ignored.is_some() && self.last_ignored.unwrap() != addr
                {
                    log::warn!("ignoring events from client {addr}");
                    self.last_ignored = Some(addr);
                }
                return;
            }
        };

        // next event can be logged as ignored again
        self.last_ignored = None;

        log::trace!("{:20} <-<-<-<------ {addr} ({handle})", event.to_string());
        let state = match self.client_manager.get_mut(handle) {
            Some(s) => s,
            None => {
                log::error!("unknown handle");
                return;
            }
        };

        // reset ttl for client and
        state.last_seen = Some(Instant::now());
        // set addr as new default for this client
        state.client.active_addr = Some(addr);

        match (event, addr) {
            (Event::Pong(), _) => { /* ignore pong events */ }
            (Event::Ping(), addr) => {
                if let Err(e) = send_event(&self.socket, Event::Pong(), addr) {
                    log::error!("udp send: {}", e);
                }
            }
            (event, addr) => {
                // tell clients that we are ready to receive events
                if let Event::Enter() = event {
                    if let Err(e) = send_event(&self.socket, Event::Leave(), addr) {
                        log::error!("udp send: {}", e);
                    }
                }
                match self.state {
                    State::Sending => {
                        if let Event::Leave() = event {
                            // ignore additional leave events that may
                            // have been sent for redundancy
                        } else {
                            // upon receiving any event, we go back to receiving mode
                            self.producer.release();
                            self.state = State::Receiving;
                        }
                    }
                    State::Receiving => {
                        // consume event
                        self.consumer.consume(event, handle).await;
                        log::trace!("{event:?} => consumer");
                    }
                    State::AwaitingLeave => {
                        // we just entered the deadzone of a client, so
                        // we need to ignore events that may still
                        // be on the way until a leave event occurs
                        // telling us the client registered the enter
                        if let Event::Leave() = event {
                            self.state = State::Sending;
                        }

                        // entering a client that is waiting for a leave
                        // event should still be possible
                        if let Event::Enter() = event {
                            self.state = State::Receiving;
                            self.producer.release();
                        }
                    }
                }
            }
        }
        // let the server know we are still alive once every second
        if state.last_replied.is_none()
            || state.last_replied.is_some()
                && state.last_replied.unwrap().elapsed() > Duration::from_secs(1)
        {
            state.last_replied = Some(Instant::now());
            if let Err(e) = send_event(&self.socket, Event::Pong(), addr) {
                log::error!("udp send: {}", e);
            }
        }
    }

    const RELEASE_MODIFIERDS: u32 = 77; // ctrl+shift+super+alt

    async fn handle_producer_event(&mut self, c: ClientHandle, mut e: Event) {
        log::trace!("producer: ({c}) {e:?}");

        if let Event::Keyboard(crate::event::KeyboardEvent::Modifiers {
            mods_depressed,
            mods_latched: _,
            mods_locked: _,
            group: _,
        }) = e
        {
            if mods_depressed == Self::RELEASE_MODIFIERDS {
                self.producer.release();
                self.state = State::Receiving;
                // send an event to release all the modifiers
                e = Event::Keyboard(KeyboardEvent::Modifiers {
                    mods_depressed: 0,
                    mods_latched: 0,
                    mods_locked: 0,
                    group: 0,
                });
            }
        }

        // get client state for handle
        let state = match self.client_manager.get_mut(c) {
            Some(state) => state,
            None => {
                // should not happen
                log::warn!("unknown client!");
                self.producer.release();
                self.state = State::Receiving;
                return;
            }
        };

        // if we just entered the client we want to send additional enter events until
        // we get a leave event
        if let State::Receiving | State::AwaitingLeave = self.state {
            self.state = State::AwaitingLeave;
            if let Some(addr) = state.client.active_addr {
                if let Err(e) = send_event(&self.socket, Event::Enter(), addr) {
                    log::error!("udp send: {}", e);
                }
            }
        }

        // otherwise we should have an address to
        // transmit events to the corrensponding client
        if let Some(addr) = state.client.active_addr {
            if let Err(e) = send_event(&self.socket, e, addr) {
                log::error!("udp send: {}", e);
            }
        }

        // if client last responded > 2 seconds ago
        // and we have not sent a ping since 500 milliseconds, send a ping

        // check if client was seen in the past 2 seconds
        if state.last_seen.is_some() && state.last_seen.unwrap().elapsed() < Duration::from_secs(2)
        {
            return;
        }

        // check if last ping is < 500ms ago
        if state.last_ping.is_some()
            && state.last_ping.unwrap().elapsed() < Duration::from_millis(500)
        {
            return;
        }

        // last seen >= 2s, last ping >= 500ms
        // -> client did not respond or a ping has not been sent for a while
        // (pings are only sent when trying to access a device!)

        // check if last ping was < 1s ago -> 500ms < last_ping < 1s
        // -> client did not respond in at least 500ms
        if state.last_ping.is_some() && state.last_ping.unwrap().elapsed() < Duration::from_secs(1)
        {
            // client unresponsive -> set state to receiving
            if self.state != State::Receiving {
                log::info!("client not responding - releasing pointer");
                self.producer.release();
                self.state = State::Receiving;
            }
        }

        // last ping > 500ms ago -> ping all interfaces
        state.last_ping = Some(Instant::now());
        for addr in state.client.addrs.iter() {
            log::debug!("pinging {addr}");
            if let Err(e) = send_event(&self.socket, Event::Ping(), *addr) {
                if e.kind() != ErrorKind::WouldBlock {
                    log::error!("udp send: {}", e);
                }
            }
        }
    }

    #[cfg(unix)]
    async fn handle_frontend_stream(&mut self, mut stream: ReadHalf<UnixStream>) {
        use std::io;

        let tx = self.frontend_tx.clone();
        tokio::task::spawn_local(async move {
            loop {
                let event = frontend::read_event(&mut stream).await;
                match event {
                    Ok(event) => tx.send(event).await.unwrap(),
                    Err(e) => {
                        if let Some(e) = e.downcast_ref::<io::Error>() {
                            if e.kind() == ErrorKind::UnexpectedEof {
                                return;
                            }
                        }
                        log::error!("error reading frontend event: {e}");
                    }
                }
            }
        });
        self.enumerate().await;
    }

    #[cfg(windows)]
    async fn handle_frontend_stream(&mut self, mut stream: ReadHalf<TcpStream>) {
        let tx = self.frontend_tx.clone();
        tokio::task::spawn_local(async move {
            loop {
                let event = frontend::read_event(&mut stream).await;
                match event {
                    Ok(event) => tx.send(event).await.unwrap(),
                    Err(e) => log::error!("error reading frontend event: {e}"),
                }
            }
        });
        self.enumerate().await;
    }

    async fn handle_frontend_event(&mut self, event: FrontendEvent) -> bool {
        log::debug!("frontend: {event:?}");
        match event {
            FrontendEvent::AddClient(hostname, port, pos) => {
                self.add_client(hostname, HashSet::new(), port, pos).await;
            }
            FrontendEvent::ActivateClient(client, active) => {
                self.activate_client(client, active).await
            }
            FrontendEvent::ChangePort(port) => {
                let current_port = self.socket.local_addr().unwrap().port();
                if current_port == port {
                    if let Err(e) = self
                        .frontend
                        .notify_all(FrontendNotify::NotifyPortChange(port, None))
                        .await
                    {
                        log::warn!("error notifying frontend: {e}");
                    }
                    return false;
                }
                let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), port);
                match UdpSocket::bind(listen_addr).await {
                    Ok(socket) => {
                        self.socket = socket;
                        if let Err(e) = self
                            .frontend
                            .notify_all(FrontendNotify::NotifyPortChange(port, None))
                            .await
                        {
                            log::warn!("error notifying frontend: {e}");
                        }
                    }
                    Err(e) => {
                        log::warn!("could not change port: {e}");
                        let port = self.socket.local_addr().unwrap().port();
                        if let Err(e) = self
                            .frontend
                            .notify_all(FrontendNotify::NotifyPortChange(
                                port,
                                Some(format!("could not change port: {e}")),
                            ))
                            .await
                        {
                            log::error!("error notifying frontend: {e}");
                        }
                    }
                }
            }
            FrontendEvent::DelClient(client) => {
                self.remove_client(client).await;
            }
            FrontendEvent::Enumerate() => self.enumerate().await,
            FrontendEvent::Shutdown() => {
                log::info!("terminating gracefully...");
                return true;
            }
            FrontendEvent::UpdateClient(client, hostname, port, pos) => {
                self.update_client(client, hostname, port, pos).await
            }
        }
        false
    }

    async fn enumerate(&mut self) {
        let clients = self
            .client_manager
            .get_client_states()
            .map(|s| (s.client.clone(), s.active))
            .collect();
        if let Err(e) = self
            .frontend
            .notify_all(FrontendNotify::Enumerate(clients))
            .await
        {
            log::error!("error notifying frontend: {e}");
        }
    }
}

async fn receive_event(
    socket: &UdpSocket,
) -> std::result::Result<(Event, SocketAddr), Box<dyn Error>> {
    let mut buf = vec![0u8; 22];
    match socket.recv_from(&mut buf).await {
        Ok((_amt, src)) => Ok((Event::try_from(buf)?, src)),
        Err(e) => Err(Box::new(e)),
    }
}

fn send_event(sock: &UdpSocket, e: Event, addr: SocketAddr) -> Result<usize> {
    log::trace!("{:20} ------>->->-> {addr}", e.to_string());
    let data: Vec<u8> = (&e).into();
    // When udp blocks, we dont want to block the event loop.
    // Dropping events is better than potentially crashing the event
    // producer.
    sock.try_send_to(&data, addr)
}
