use std::{error::Error, io::Result};
use log;
use mio::{Events, Poll, Interest, Token, net::UdpSocket};
#[cfg(not(windows))]
use mio_signals::{Signals, Signal, SignalSet};

use std::{net::SocketAddr, io::ErrorKind};

use crate::{client::{ClientEvent, ClientManager, Position}, consumer::EventConsumer, producer::EventProducer, frontend::{FrontendEvent, FrontendAdapter}};
use super::Event;

pub struct Server {
    poll: Poll,
    socket: UdpSocket,
    producer: Box<dyn EventProducer>,
    consumer: Box<dyn EventConsumer>,
    #[cfg(not(windows))]
    signals: Signals,
    frontend: FrontendAdapter,
    client_manager: ClientManager,
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
        poll.registry().register(&mut socket, UDP_RX, Interest::READABLE)?;
        poll.registry().register(&mut producer, PRODUCER_RX, Interest::READABLE)?;
        poll.registry().register(&mut frontend, FRONTEND_RX, Interest::READABLE)?;

        // create client manager
        let client_manager = ClientManager::new();
        Ok(Server {
            poll, socket, consumer, producer,
            #[cfg(not(windows))]
            signals, frontend,
            client_manager,
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

    pub fn add_client(&mut self, addr: Vec<SocketAddr>, pos: Position) {
        let client = self.client_manager.add_client(addr, pos);
        self.producer.notify(ClientEvent::Create(client, pos));
        self.consumer.notify(ClientEvent::Create(client, pos));
    }

    fn handle_udp_rx(&mut self) {
        loop {
            match Self::receive_event(&self.socket) {
                Ok((event, addr)) => {
                    log::debug!("{addr}: {event:?}");
                    if let Event::Release() = event {
                        self.producer.release();
                        return;
                    }
                    if let Some(client_handle) = self.client_manager.get_client(addr) {
                        self.consumer.consume(event, client_handle);
                        self.client_manager.set_default_addr(client_handle, addr);
                    } else {
                        log::warn!("ignoring event from client {addr:?}");
                    }
                }
                Err(e) => {
                    if e.is::<std::io::Error>() {
                        match e.downcast_ref::<std::io::Error>().unwrap().kind() {
                            ErrorKind::WouldBlock => return,
                            _ => continue,
                        }
                    }
                    log::error!("{}", e);
                    return;
                },
            }
        }
    }

    fn handle_producer_rx(&mut self) {
        let events = self.producer.read_events();
        events.into_iter().for_each(|(c, e)| {
            if let Some(addr) = self.client_manager.get_active_addr(c) {
                Self::send_event(&self.socket, e, addr);
            } else {
                log::error!("unknown client: id {c}");
            }
        })
    }

    fn handle_frontend_rx(&mut self) -> bool {
        loop {
            match self.frontend.read_event() {
                Ok(event) => match event {
                    FrontendEvent::RequestPortChange(_) => todo!(),
                    FrontendEvent::RequestClientAdd(addr, pos) => {
                        self.add_client(vec![addr], pos);
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

    fn send_event(tx: &UdpSocket, e: Event, addr: SocketAddr) {
        let data: Vec<u8> = (&e).into();
        // We are currently abusing a blocking send to get the lowest possible latency.
        // It may be better to set the socket to non-blocking and only send when ready.
        if let Err(e) = tx.send_to(&data[..], addr) {
            log::error!("udp send: {}", e);
        }
    }

    fn receive_event(rx: &UdpSocket) -> std::result::Result<(Event, SocketAddr), Box<dyn Error>> {
        let mut buf = vec![0u8; 22];
        match rx.recv_from(&mut buf) {
            Ok((_amt, src)) => Ok((Event::try_from(buf)?, src)),
            Err(e) => Err(Box::new(e)),
        }
    }
}
