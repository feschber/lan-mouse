use std::{error::Error, io::Result, collections::HashSet, time::{Duration, Instant}, net::IpAddr};
use log;
use mio::{Events, Poll, Interest, Token, net::UdpSocket, event::Source};
#[cfg(not(windows))]
use mio_signals::{Signals, Signal, SignalSet};

use std::{net::SocketAddr, io::ErrorKind};

use crate::{client::{ClientEvent, ClientManager, Position, ClientHandle}, consumer::EventConsumer, producer::EventProducer, frontend::{FrontendEvent, FrontendListener, FrontendNotify}, dns::{self, DnsResolver}};
use super::Event;

/// keeps track of state to prevent a feedback loop
/// of continuously sending and receiving the same event.
#[derive(Eq, PartialEq)]
enum State {
    Sending,
    Receiving,
}

pub struct Server {
    poll: Poll,
    socket: UdpSocket,
    producer: Box<dyn EventProducer>,
    consumer: Box<dyn EventConsumer>,
    resolver: DnsResolver,
    #[cfg(not(windows))]
    signals: Signals,
    frontend: FrontendListener,
    client_manager: ClientManager,
    state: State,
    next_token: usize,
}

const UDP_RX: Token = Token(0);
const FRONTEND_RX: Token = Token(1);
const PRODUCER_RX: Token = Token(2);
#[cfg(not(windows))]
const SIGNAL: Token = Token(3);

const MAX_TOKEN: usize = 4;

impl Server {
    pub fn new(
        port: u16,
        mut producer: Box<dyn EventProducer>,
        consumer: Box<dyn EventConsumer>,
        mut frontend: FrontendListener,
    ) -> anyhow::Result<Self> {
        // bind the udp socket
        let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), port);
        let mut socket = UdpSocket::bind(listen_addr)?;

        // create dns resolver
        let resolver = dns::DnsResolver::new()?;

        // register event sources
        let poll = Poll::new()?;

        // hand signal handling over to the event loop
        #[cfg(not(windows))]
        let mut signals = Signals::new(SignalSet::all())?;

        #[cfg(not(windows))]
        poll.registry().register(&mut signals, SIGNAL, Interest::READABLE)?;
        poll.registry().register(&mut socket, UDP_RX, Interest::WRITABLE)?;
        poll.registry().register(&mut producer, PRODUCER_RX, Interest::READABLE)?;
        poll.registry().register(&mut frontend, FRONTEND_RX, Interest::READABLE)?;

        // create client manager
        let client_manager = ClientManager::new();
        Ok(Server {
            poll, socket, consumer, producer,
            resolver,
            #[cfg(not(windows))]
            signals, frontend,
            client_manager,
            state: State::Receiving,
            next_token: MAX_TOKEN,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        let mut events = Events::with_capacity(10);
        loop {
            match self.poll.poll(&mut events, None) {
                Ok(()) => (),
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
            for event in &events {
                if !event.is_readable() { continue }
                match event.token() {
                    UDP_RX => self.handle_udp_rx(),
                    PRODUCER_RX => self.handle_producer_rx(),
                    FRONTEND_RX => self.handle_frontend_incoming(),
                    #[cfg(not(windows))]
                    SIGNAL => if self.handle_signal() { return Ok(()) },
                    _ => if self.handle_frontend_event(event.token()) { return Ok(()) },
                }
            }
        }
    }

    pub fn add_client(&mut self, hostname: Option<String>, mut addr: HashSet<IpAddr>, port: u16, pos: Position) -> ClientHandle {
        let ips = if let Some(hostname) = hostname.as_ref() {
            HashSet::from_iter(self.resolver.resolve(hostname.as_str()).ok().iter().flatten().cloned())
        } else {
            HashSet::new()
        };
        addr.extend(ips.iter());
        log::info!("adding client [{}]{} @ {:?}", pos, hostname.as_deref().unwrap_or(""), &ips);
        let client = self.client_manager.add_client(hostname.clone(), addr, port, pos);
        log::debug!("add_client {client}");
        let notify = FrontendNotify::NotifyClientCreate(client, hostname, port, pos);
        if let Err(e) = self.frontend.notify_all(notify) {
            log::error!("{e}");
        };
        client
    }

    pub fn activate_client(&mut self, client: ClientHandle, active: bool) {
        if let Some(state) = self.client_manager.get_mut(client) {
            state.active = active;
            if state.active {
                self.producer.notify(ClientEvent::Create(client, state.client.pos));
                self.consumer.notify(ClientEvent::Create(client, state.client.pos));
            } else {
                self.producer.notify(ClientEvent::Destroy(client));
                self.consumer.notify(ClientEvent::Destroy(client));
            }
        }
    }

    pub fn remove_client(&mut self, client: ClientHandle) -> Option<ClientHandle> {
        self.producer.notify(ClientEvent::Destroy(client));
        self.consumer.notify(ClientEvent::Destroy(client));
        if let Some(client) = self.client_manager.remove_client(client).map(|s| s.client.handle) {
            let notify = FrontendNotify::NotifyClientDelete(client);
            log::debug!("{notify:?}");
            if let Err(e) = self.frontend.notify_all(notify) {
                log::error!("{e}");
            }
            Some(client)
        } else {
            None
        }
    }

    pub fn update_client(
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
            self.consumer.notify(ClientEvent::Destroy(client));
            self.producer.notify(ClientEvent::Create(client, pos));
            self.consumer.notify(ClientEvent::Create(client, pos));
        }

        // update port
        if state.client.port != port {
            state.client.port = port;
            state.client.addrs = state.client.addrs
                .iter()
                .cloned()
                .map(|mut a| { a.set_port(port); a })
                .collect();
            state.client.active_addr.map(|mut a| { a.set_port(port); a });
        }

        // update hostname
        if state.client.hostname != hostname {
            state.client.addrs = HashSet::new();
            state.client.active_addr = None;
            state.client.hostname = hostname;
            if let Some(hostname) = state.client.hostname.as_ref() {
                if let Ok(ips) = self.resolver.resolve(hostname.as_str()) {
                    let addrs = ips.iter().map(|i| SocketAddr::new(*i, port));
                    state.client.addrs = HashSet::from_iter(addrs);
                }
            }
        }
    }

    fn handle_udp_rx(&mut self) {
        loop {
            let (event, addr) = match self.receive_event() {
                Ok(e) => e,
                Err(e) => {
                    if e.is::<std::io::Error>() {
                        if let ErrorKind::WouldBlock = e.downcast_ref::<std::io::Error>()
                            .unwrap()
                            .kind() {
                            return
                        }
                    }
                    log::error!("{}", e);
                    continue
                }
            };
            log::trace!("{:20} <-<-<-<------ {addr}", event.to_string());

            // get handle for addr
            let handle = match self.client_manager.get_client(addr) {
                Some(a) => a,
                None => {
                    log::warn!("ignoring event from client {addr:?}");
                    continue
                }
            };
            let state = match self.client_manager.get_mut(handle) {
                Some(s) => s,
                None => {
                    log::error!("unknown handle");
                    continue
                }
            };

            // reset ttl for client and 
            state.last_seen = Some(Instant::now());
            // set addr as new default for this client
            state.client.active_addr = Some(addr);
            match (event, addr) {
                (Event::Pong(), _) => {},
                (Event::Ping(), addr) => {
                    if let Err(e) = Self::send_event(&self.socket, Event::Pong(), addr) {
                        log::error!("udp send: {}", e);
                    }
                    // we release the mouse here,
                    // since its very likely, that we wont get a release event
                    self.producer.release();
                }
                (event, addr) => {
                    match self.state {
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
                            self.consumer.consume(event, handle);

                            // let the server know we are still alive once every second
                            let last_replied = state.last_replied;
                            if  last_replied.is_none() 
                            || last_replied.is_some()
                            && last_replied.unwrap().elapsed() > Duration::from_secs(1) {
                                state.last_replied = Some(Instant::now());
                                if let Err(e) = Self::send_event(&self.socket, Event::Pong(), addr) {
                                    log::error!("udp send: {}", e);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn handle_producer_rx(&mut self) {
        let events = self.producer.read_events();
        let mut should_release = false;
        for (c, e) in events.into_iter() {
            // in receiving state, only release events
            // must be transmitted
            if let Event::Release() = e {
                self.state = State::Sending;
            }

            let state = match self.client_manager.get_mut(c) {
                Some(state) => state,
                None => {
                    log::warn!("unknown client!");
                    continue
                }
            };
            // otherwise we should have an address to send to
            // transmit events to the corrensponding client
            if let Some(addr) = state.client.active_addr {
                log::trace!("{:20} ------>->->-> {addr}", e.to_string());
                if let Err(e) = Self::send_event(&self.socket, e, addr) {
                    log::error!("udp send: {}", e);
                }
            }

            // if client last responded > 2 seconds ago
            // and we have not sent a ping since 500 milliseconds,
            // send a ping
            if state.last_seen.is_some()
            && state.last_seen.unwrap().elapsed() < Duration::from_secs(2) {
                continue
            }

            // client last seen > 500ms ago
            if state.last_ping.is_some()
            && state.last_ping.unwrap().elapsed() < Duration::from_millis(500) {
                continue
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
                if let Err(e) = Self::send_event(&self.socket, Event::Ping(), *addr) {
                    if e.kind() != ErrorKind::WouldBlock {
                        log::error!("udp send: {}", e);
                    }
                }
                // send additional release event, in case client is still in sending mode
                if let Err(e) = Self::send_event(&self.socket, Event::Release(), *addr) {
                    if e.kind() != ErrorKind::WouldBlock {
                        log::error!("udp send: {}", e);
                    }
                }
            }
        }

        if should_release && self.state != State::Receiving {
            log::info!("client not responding - releasing pointer");
            self.producer.release();
            self.state = State::Receiving;
        }

    }

    fn handle_frontend_incoming(&mut self) {
        loop {
            let token = self.fresh_token();
            let poll = &mut self.poll;
            match self.frontend.handle_incoming(|s, i| {
                poll.registry().register(s, token, i)?;
                Ok(token)
            }) {
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) =>  {
                    log::error!("{e}");
                    break
                }
                _ => continue,
            }
        }
        // notify new frontend connections of current clients
        self.enumerate();
    }

    fn handle_frontend_event(&mut self, token: Token) -> bool {
        loop {
            let event = match self.frontend.read_event(token) {
                Ok(event) => event,
                Err(e) if e.kind() == ErrorKind::WouldBlock => return false,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => {
                    log::error!("{e}");
                    return false;
                }
            };
            if let Some(event) = event {
                log::debug!("frontend: {event:?}");
                    match event {
                    FrontendEvent::AddClient(hostname, port, pos) => {
                        self.add_client(hostname, HashSet::new(), port, pos);
                    }
                    FrontendEvent::ActivateClient(client, active) => {
                        self.activate_client(client, active);
                    }
                    FrontendEvent::DelClient(client) => {
                        self.remove_client(client);
                    }
                    FrontendEvent::UpdateClient(client, hostname, port, pos) => {
                        self.update_client(client, hostname, port, pos);
                    }
                    FrontendEvent::Enumerate() => self.enumerate(),
                    FrontendEvent::Shutdown() => {
                        log::info!("terminating gracefully...");
                        return true;
                    },
                }
            }
        }
    }

    fn enumerate(&mut self) {
        let clients = self.client_manager.enumerate();
        if let Err(e) = self.frontend.notify_all(FrontendNotify::Enumerate(clients)) {
            log::error!("{e}");
        }
    }

    #[cfg(not(windows))]
    fn handle_signal(&mut self) -> bool {
        #[cfg(windows)]
        return false;
        #[cfg(not(windows))]
        loop {
            match self.signals.receive() {
                Err(e) if e.kind() == ErrorKind::WouldBlock => return false,
                Err(e) => {
                    log::error!("error reading signal: {e}");
                    return false;
                }
                Ok(Some(Signal::Interrupt) | Some(Signal::Terminate)) => {
                    // terminate on SIG_INT or SIG_TERM
                    log::info!("terminating gracefully...");
                    return true;
                },
                Ok(Some(signal)) => {
                    log::info!("ignoring signal {signal:?}");
                },
                Ok(None) => return false,
            }
        }
    }

    fn send_event(sock: &UdpSocket, e: Event, addr: SocketAddr) -> Result<usize> {
        let data: Vec<u8> = (&e).into();
        // We are currently abusing a blocking send to get the lowest possible latency.
        // It may be better to set the socket to non-blocking and only send when ready.
        sock.send_to(&data[..], addr)
    }

    fn receive_event(&self) -> std::result::Result<(Event, SocketAddr), Box<dyn Error>> {
        let mut buf = vec![0u8; 22];
        match self.socket.recv_from(&mut buf) {
            Ok((_amt, src)) => Ok((Event::try_from(buf)?, src)),
            Err(e) => Err(Box::new(e)),
        }
    }

    fn fresh_token(&mut self) ->  Token {
        let token = self.next_token as usize;
        self.next_token += 1;
        Token(token)
    }

    pub fn register_frontend(&mut self, source: &mut dyn Source, interests: Interest) -> Result<Token> {
        let token = self.fresh_token();
        self.poll.registry().register(source, token, interests)?;
        Ok(token)
    }
}
