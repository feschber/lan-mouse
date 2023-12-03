use std::{error::Error, io::Result, collections::HashSet, time::{Duration, Instant}, net::IpAddr};
use log;
use tokio::{net::UdpSocket, io::ReadHalf, signal, sync::mpsc::{Sender, Receiver}};
use futures::stream::StreamExt;

#[cfg(unix)]
use tokio::net::UnixStream;

#[cfg(windows)]
use tokio::net::TcpStream;

use std::{net::SocketAddr, io::ErrorKind};

use crate::{client::{ClientEvent, ClientManager, Position, ClientHandle}, consumer::EventConsumer, producer::EventProducer, frontend::{FrontendEvent, FrontendListener, FrontendNotify, self}, dns::{self, DnsResolver}};
// use crate::event::PointerEvent;
use super::Event;

/// keeps track of state to prevent a feedback loop
/// of continuously sending and receiving the same event.
#[derive(Eq, PartialEq)]
enum State {
    Sending,
    Receiving,
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
}

impl Server {
    pub async fn new(
        port: u16,
        frontend: FrontendListener,
        consumer: Box<dyn EventConsumer>,
        producer: Box<dyn EventProducer>,
    ) -> anyhow::Result<Self> {

        // create dns resolver
        let resolver = dns::DnsResolver::new().await?;

        // bind the udp socket
        let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), port);
        let socket = UdpSocket::bind(listen_addr).await?;
        let (frontend_tx, frontend_rx) = tokio::sync::mpsc::channel(1);

        // create client manager
        let client_manager = ClientManager::new();
        Ok(Server {
            frontend,
            consumer,
            producer,
            resolver,
            socket,
            client_manager,
            state: State::Receiving,
            frontend_rx,
            frontend_tx,
        })
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {

        loop {
            log::trace!("polling ...");
            tokio::select! {
                // safety: cancellation safe
                udp_event = receive_event(&self.socket) => {
                    log::trace!("-> receive_event");
                    match udp_event {
                        Ok(e) => self.handle_udp_rx(e).await,
                        Err(e) => log::error!("error reading event: {e}"),
                    }
                }
                // safety: cancellation safe
                res = self.producer.next() => {
                    log::trace!("-> producer.next()");
                    match res {
                        Some(Ok((client, event))) => {
                            self.handle_producer_event(client,event).await;
                        },
                        Some(Err(e)) => log::error!("{e}"),
                        _ => break,
                    }
                }
                // safety: cancellation safe
                stream = self.frontend.accept() => {
                    log::trace!("-> frontend.accept()");
                    match stream {
                        Ok(s) => self.handle_frontend_stream(s).await,
                        Err(e) => log::error!("error connecting to frontend: {e}"),
                    }
                }
                // safety: cancellation safe
                frontend_event = self.frontend_rx.recv() => {
                    log::trace!("-> frontend.recv()");
                    if let Some(event) = frontend_event {
                        if self.handle_frontend_event(event).await {
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep_until(tokio::time::Instant::now() + Duration::from_millis(50)) => {
                    let event = Event::Keyboard(crate::event::KeyboardEvent::Key { time: 0, key: 30, state: 1 });
                    self.consumer.consume(event, 0).await;
                    let event = Event::Keyboard(crate::event::KeyboardEvent::Key { time: 0, key: 30, state: 0 });
                    self.consumer.consume(event, 0).await;
                }
                // safety: cancellation safe
                e = self.consumer.dispatch() => {
                    log::trace!("-> consumer.dispatch()");
                    if let Err(e) = e {
                        return Err(e);
                    }
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

    pub async fn add_client(&mut self, hostname: Option<String>, mut addr: HashSet<IpAddr>, port: u16, pos: Position) -> ClientHandle {
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
        log::info!("adding client [{}]{} @ {:?}", pos, hostname.as_deref().unwrap_or(""), &ips);
        let client = self.client_manager.add_client(hostname.clone(), addr, port, pos);
        log::debug!("add_client {client}");
        let notify = FrontendNotify::NotifyClientCreate(client, hostname, port, pos);
        if let Err(e) = self.frontend.notify_all(notify).await {
            log::error!("{e}");
        };
        client
    }

    pub async fn activate_client(&mut self, client: ClientHandle, active: bool) {
        if let Some(state) = self.client_manager.get_mut(client) {
            state.active = active;
            if state.active {
                self.producer.notify(ClientEvent::Create(client, state.client.pos));
                self.consumer.notify(ClientEvent::Create(client, state.client.pos)).await;
            } else {
                self.producer.notify(ClientEvent::Destroy(client));
                self.consumer.notify(ClientEvent::Destroy(client)).await;
            }
        }
    }

    pub async fn remove_client(&mut self, client: ClientHandle) -> Option<ClientHandle> {
        self.producer.notify(ClientEvent::Destroy(client));
        self.consumer.notify(ClientEvent::Destroy(client)).await;
        if let Some(client) = self.client_manager.remove_client(client).map(|s| s.client.handle) {
            let notify = FrontendNotify::NotifyClientDelete(client);
            log::debug!("{notify:?}");
            if let Err(e) = self.frontend.notify_all(notify).await {
                log::error!("{e}");
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
            return
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
            state.client.addrs = state.client.addrs
                .iter()
                .cloned()
                .map(|mut a| { a.set_port(port); a })
                .collect();
            state.client.active_addr.map(|a| { SocketAddr::new(a.ip(), port) });
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
                log::warn!("ignoring event from client {addr:?}");
                return;
            }
        };

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
            (Event::Pong(), _) => {},
            (Event::Ping(), addr) => {
                if let Err(e) = send_event(&self.socket, Event::Pong(), addr).await {
                    log::error!("udp send: {}", e);
                }
                // we release the mouse here,
                // since its very likely, that we wont get a release event
                self.producer.release();
            }
            (event, addr) => match self.state {
                State::Sending => {
                    // in sending state, we dont want to process
                    // any events to avoid feedback loops,
                    // therefore we tell the event producer
                    // to release the pointer and move on
                    // first event -> release pointer
                    if let Event::Release() = event {
                        log::debug!("releasing pointer ...");
                        self.producer.release();
                        self.state = State::Receiving;
                    }
                }
                State::Receiving => {
                    // consume event
                    self.consumer.consume(event, handle).await;

                    // let the server know we are still alive once every second
                    let last_replied = state.last_replied;
                    if  last_replied.is_none() 
                    || last_replied.is_some()
                    && last_replied.unwrap().elapsed() > Duration::from_secs(1) {
                        state.last_replied = Some(Instant::now());
                        if let Err(e) = send_event(&self.socket, Event::Pong(), addr).await {
                            log::error!("udp send: {}", e);
                        }
                    }
                }
            }
        }
    }

    async fn handle_producer_event(&mut self, c: ClientHandle, e: Event) {
        let mut should_release = false;
        // in receiving state, only release events
        // must be transmitted
        if let Event::Release() = e {
            self.state = State::Sending;
        }

        log::trace!("producer: ({c}) {e:?}");
        let state = match self.client_manager.get_mut(c) {
            Some(state) => state,
            None => {
                log::warn!("unknown client!");
                return
            }
        };
        // otherwise we should have an address to send to
        // transmit events to the corrensponding client
        if let Some(addr) = state.client.active_addr {
            if let Err(e) = send_event(&self.socket, e, addr).await {
                log::error!("udp send: {}", e);
            }
        }

        // if client last responded > 2 seconds ago
        // and we have not sent a ping since 500 milliseconds,
        // send a ping
        if state.last_seen.is_some()
        && state.last_seen.unwrap().elapsed() < Duration::from_secs(2) {
            return
        }

        // client last seen > 500ms ago
        if state.last_ping.is_some()
        && state.last_ping.unwrap().elapsed() < Duration::from_millis(500) {
            return
        }

        // release mouse if client didnt respond to the first ping
        if state.last_ping.is_some()
        && state.last_ping.unwrap().elapsed() < Duration::from_secs(1) {
            should_release = true;
        }

        // last ping > 500ms ago -> ping all interfaces
        state.last_ping = Some(Instant::now());
        for addr in state.client.addrs.iter() {
            log::debug!("pinging {addr}");
            if let Err(e) = send_event(&self.socket, Event::Ping(), *addr).await {
                if e.kind() != ErrorKind::WouldBlock {
                    log::error!("udp send: {}", e);
                }
            }
            // send additional release event, in case client is still in sending mode
            if let Err(e) = send_event(&self.socket, Event::Release(), *addr).await {
                if e.kind() != ErrorKind::WouldBlock {
                    log::error!("udp send: {}", e);
                }
            }
        }

        if should_release && self.state != State::Receiving {
            log::info!("client not responding - releasing pointer");
            self.producer.release();
            self.state = State::Receiving;
        }
    }

    #[cfg(unix)]
    async fn handle_frontend_stream(&mut self, mut stream: ReadHalf<UnixStream>) {
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
            FrontendEvent::AddClient(hostname, port, pos) => { self.add_client(hostname, HashSet::new(), port, pos).await; },
            FrontendEvent::ActivateClient(client, active) => self.activate_client(client, active).await,
            FrontendEvent::ChangePort(port) => {
                let current_port = self.socket.local_addr().unwrap().port();
                if current_port == port {
                    if let Err(e) = self.frontend.notify_all(FrontendNotify::NotifyPortChange(port, None)).await {
                        log::warn!("error notifying frontend: {e}");
                    }
                    return false;
                }
                let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), port);
                match UdpSocket::bind(listen_addr).await {
                    Ok(socket) => {
                        self.socket = socket;
                        if let Err(e) = self.frontend.notify_all(FrontendNotify::NotifyPortChange(port, None)).await {
                            log::warn!("error notifying frontend: {e}");
                        }
                    },
                    Err(e) => {
                        log::warn!("could not change port: {e}");
                        let port = self.socket.local_addr().unwrap().port();
                        if let Err(e) = self.frontend.notify_all(FrontendNotify::NotifyPortChange(port, Some(format!("could not change port: {e}")))).await {
                            log::error!("error notifying frontend: {e}");
                        }
                    }
                }
            },
            FrontendEvent::DelClient(client) => { self.remove_client(client).await; },
            FrontendEvent::Enumerate() => self.enumerate().await,
            FrontendEvent::Shutdown() => {
                log::info!("terminating gracefully...");
                return true;
            },
            FrontendEvent::UpdateClient(client, hostname, port, pos) => self.update_client(client, hostname, port, pos).await,
        }
        false
    }

    async fn enumerate(&mut self) {
        let clients = self.client_manager.enumerate();
        if let Err(e) = self.frontend.notify_all(FrontendNotify::Enumerate(clients)).await {
            log::error!("{e}");
        }
    }
}

async fn receive_event(socket: &UdpSocket) -> std::result::Result<(Event, SocketAddr), Box<dyn Error>> {
    log::trace!("receive_event");
    let mut buf = vec![0u8; 22];
    match socket.recv_from(&mut buf).await {
        Ok((_amt, src)) => Ok((Event::try_from(buf)?, src)),
        Err(e) => Err(Box::new(e)),
    }
}


async fn send_event(sock: &UdpSocket, e: Event, addr: SocketAddr) -> Result<usize> {
    log::trace!("{:20} ------>->->-> {addr}", e.to_string());
    let data: Vec<u8> = (&e).into();
    // We are currently abusing a blocking send to get the lowest possible latency.
    // It may be better to set the socket to non-blocking and only send when ready.
    sock.send_to(&data[..], addr).await
}

