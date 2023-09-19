use std::{error::Error, io::Result, collections::HashSet, time::Duration};
use log;
use mio::{Events, Poll, Interest, Token, net::UdpSocket};
#[cfg(not(windows))]
use mio_signals::{Signals, Signal, SignalSet};

use std::{net::SocketAddr, io::ErrorKind};

use crate::{client::{ClientEvent, ClientManager, Position}, consumer::EventConsumer, producer::EventProducer, frontend::{FrontendEvent, FrontendAdapter}};
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
    #[cfg(not(windows))]
    signals: Signals,
    frontend: FrontendAdapter,
    client_manager: ClientManager,
    state: State,
}

const UDP_RX: Token = Token(0);
const FRONTEND_RX: Token = Token(1);
const PRODUCER_RX: Token = Token(2);
#[cfg(not(windows))]
const SIGNAL: Token = Token(3);

impl Server {
    pub fn new(
        port: u16,
        mut producer: Box<dyn EventProducer>,
        consumer: Box<dyn EventConsumer>,
        mut frontend: FrontendAdapter,
    ) -> Result<Self> {
        // bind the udp socket
        let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), port);
        let mut socket = UdpSocket::bind(listen_addr)?;

        // register event sources
        let poll = Poll::new()?;

        // hand signal handling over to the event loop
        #[cfg(not(windows))]
        let mut signals = Signals::new(SignalSet::all())?;

        #[cfg(not(windows))]
        poll.registry().register(&mut signals, SIGNAL, Interest::READABLE)?;
        poll.registry().register(&mut socket, UDP_RX, Interest::READABLE | Interest::WRITABLE)?;
        poll.registry().register(&mut producer, PRODUCER_RX, Interest::READABLE)?;
        poll.registry().register(&mut frontend, FRONTEND_RX, Interest::READABLE)?;

        // create client manager
        let client_manager = ClientManager::new();
        Ok(Server {
            poll, socket, consumer, producer,
            #[cfg(not(windows))]
            signals, frontend,
            client_manager,
            state: State::Receiving,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        let mut events = Events::with_capacity(10);
        loop {
            self.poll.poll(&mut events, None)?;
            for event in &events {
                if !event.is_readable() { continue }
                match event.token() {
                    UDP_RX => self.handle_udp_rx(),
                    PRODUCER_RX => self.handle_producer_rx(),
                    FRONTEND_RX => if self.handle_frontend_rx() { return Ok(()) },
                    #[cfg(not(windows))]
                    SIGNAL => if self.handle_signal() { return Ok(()) },
                    _ => panic!("what happened here?")
                }
            }
        }
    }

    pub fn add_client(&mut self, addr: HashSet<SocketAddr>, pos: Position) {
        let client = self.client_manager.add_client(addr, pos);
        self.producer.notify(ClientEvent::Create(client, pos));
        self.consumer.notify(ClientEvent::Create(client, pos));
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

            // get handle for addr
            let handle = match self.client_manager.get_client(addr) {
                Some(a) => a,
                None => {
                    log::warn!("ignoring event from client {addr:?}");
                    continue
                }
            };

            // reset ttl for client and set addr as new default for this client
            self.client_manager.reset_last_seen(handle);
            self.client_manager.set_default_addr(handle, addr);
            match (event, addr) {
                (Event::Release(), _) => {},
                (Event::Pong(), addr) => log::debug!("{addr}: pong"),
                (Event::Ping(), addr) => {
                    if let Err(e) = Self::send_event(&self.socket, Event::Pong(), addr) {
                        log::error!("udp send: {}", e);
                    }
                }
                (event, addr) => {
                    log::debug!("{addr}: {event:?}");
                    match self.state {
                        State::Sending => {
                            // in sending state, we dont want to process
                            // any events to avoid feedback loops,
                            // therefore we tell the event producer
                            // to release the pointer and move on
                            // first event -> release pointer
                            log::debug!("releasing pointer ...");
                            self.producer.release();
                            self.state = State::Receiving;
                        }
                        State::Receiving => {
                            // consume event
                            self.consumer.consume(event, handle);

                            // let the server know we are still alive once every second
                            let last_replied = self.client_manager.last_replied(handle);
                            if  last_replied.is_none() 
                            || last_replied.is_some() && last_replied.unwrap() > Duration::from_secs(1) {
                                self.client_manager.reset_last_replied(handle);
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
        for (c, e) in events.into_iter() {
            // in receiving state, only release events
            // must be transmitted
            match self.state {
                State::Receiving => self.state = State::Sending,
                State::Sending => {
                    // otherwise we should have an address to send to
                    // transmit events to the corrensponding client
                    if let Some(addr) = self.client_manager.get_active_addr(c) {
                        if let Err(e) = Self::send_event(&self.socket, e, addr) {
                            log::error!("udp send: {}", e);
                        }
                    }

                    // if client last responded > 2 seconds ago
                    // and we have not sent a ping since 500 milliseconds,
                    // send a ping
                    let last_seen = self.client_manager.last_seen(c);
                    let last_ping = self.client_manager.last_ping(c);
                    if last_seen.is_some() && last_seen.unwrap() < Duration::from_secs(2) {
                        continue
                    }

                    // client last seen > 500ms ago
                    if last_ping.is_some() && last_ping.unwrap() < Duration::from_millis(500) {
                        continue
                    }

                    // last ping > 500ms ago -> ping all interfaces
                    self.client_manager.reset_last_ping(c);
                    if let Some(iter) = self.client_manager.get_addrs(c) {
                        for addr in iter {
                            log::debug!("pinging {addr}");
                            if let Err(e) = Self::send_event(&self.socket, Event::Ping(), addr) {
                                if e.kind() != ErrorKind::WouldBlock {
                                    log::error!("udp send: {}", e);
                                }
                            }
                        }
                    } else {
                        // TODO should repeat dns lookup
                    }
                }
            }
        }
    }

    fn handle_frontend_rx(&mut self) -> bool {
        loop {
            match self.frontend.read_event() {
                Ok(event) => match event {
                    FrontendEvent::RequestPortChange(_) => todo!(),
                    FrontendEvent::RequestClientAdd(addr, pos) => {
                        self.add_client(HashSet::from_iter(&mut [addr].into_iter()), pos);
                    }
                    FrontendEvent::RequestClientDelete(_) => todo!(),
                    FrontendEvent::RequestClientUpdate(_) => todo!(),
                    FrontendEvent::RequestShutdown() => {
                        log::info!("terminating gracefully...");
                        return true;
                    },
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => return false,
                Err(e) => {
                    log::error!("frontend: {e}");
                }
            }
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
}
