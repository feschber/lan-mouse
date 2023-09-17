use std::io::Result;
use log;
use mio::{Events, Poll, Interest, Token, net::UdpSocket};
#[cfg(not(windows))]
use mio_signals::{Signals, Signal, SignalSet};

use std::{
    collections::HashMap,
    error::Error,
    net::SocketAddr, io::ErrorKind,
};

use crate::{client::{ClientManager, ClientHandle, ClientEvent}, consumer::EventConsumer, producer::EventProducer, frontend::{FrontendEvent, FrontendAdapter}};
use super::Event;

pub struct Server {
    poll: Poll,
    socket: UdpSocket,
    producer: Box<dyn EventProducer>,
    consumer: Box<dyn EventConsumer>,
    #[cfg(not(windows))]
    signals: Signals,
    frontend: FrontendAdapter,
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
        let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), port);
        let mut socket = UdpSocket::bind(listen_addr)?;

        let poll = Poll::new()?;
        poll.registry().register(&mut socket, UDP_RX, Interest::READABLE)?;
        poll.registry().register(&mut producer, PRODUCER_RX, Interest::READABLE)?;
        poll.registry().register(&mut frontend, FRONTEND_RX, Interest::READABLE)?;

        // hand signal handing over to event loop
        #[cfg(not(windows))]
        let mut signals = Signals::new(SignalSet::all())?;
        #[cfg(not(windows))]
        poll.registry().register(&mut signals, SIGNAL, Interest::READABLE)?;
        Ok(Server {
            poll,
            socket,
            consumer,
            producer,
            #[cfg(not(windows))]
            signals,
            frontend,
        })
    }

    pub fn run(
        &mut self,
        client_manager: ClientManager,
    ) -> Result<()> {

        let mut client_for_socket: HashMap<SocketAddr, ClientHandle> = HashMap::new();
        let mut socket_for_client: HashMap<ClientHandle, SocketAddr> = HashMap::new();

        let mut events = Events::with_capacity(10);

        loop {
            self.poll.poll(&mut events, None)?;
            for event in &events {
                if !event.is_readable() { continue; }

                match event.token() {
                    UDP_RX => {
                        loop {
                            match Self::receive_event(&self.socket) {
                                Ok((event, addr)) => {
                                    log::debug!("{addr}: {event:?}");
                                    if let Event::Release() = event {
                                        self.producer.release();
                                    } else {
                                        if let Some(client_handle) = client_for_socket.get(&addr) {
                                            self.consumer.consume(event, *client_handle);
                                        } else {
                                            log::warn!("ignoring event from client {addr:?}");
                                        }
                                    }
                                }
                                Err(e) => {
                                    if e.is::<std::io::Error>() && e.downcast_ref::<std::io::Error>().unwrap().kind() == ErrorKind::WouldBlock {
                                        break;
                                    } else {
                                        log::error!("{}", e);
                                    }
                                },
                            }
                        }
                    },
                    PRODUCER_RX => {
                        let events = self.producer.read_events();
                        events.into_iter().for_each(|(c, e)| {
                            log::debug!("wayland event: {e:?}");
                            if let Some(addr) = socket_for_client.get(&c) {
                                Self::send_event(&self.socket, e, *addr);
                            } else {
                                log::error!("unknown client: id {c}");
                            }
                        })
                    },
                    FRONTEND_RX => {
                        loop {
                            match self.frontend.read_event() {
                                Ok(event) => match event {
                                    FrontendEvent::RequestPortChange(_) => todo!(),
                                    FrontendEvent::RequestClientAdd(addr, pos) => {
                                        let client = client_manager.register_client(addr, pos);
                                        socket_for_client.insert(client.handle, addr);
                                        client_for_socket.insert(addr, client.handle);
                                        self.producer.notify(ClientEvent::Create(client));
                                        self.consumer.notify(ClientEvent::Create(client));
                                    }
                                    FrontendEvent::RequestClientDelete(_) => todo!(),
                                    FrontendEvent::RequestClientUpdate(_) => todo!(),
                                    FrontendEvent::RequestShutdown() => {
                                        log::info!("terminating gracefully...");
                                        return Ok(());
                                    },
                                }
                                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                                Err(e) => {
                                    log::error!("frontend: {e}");
                                }
                            }
                        }
                    }
                    #[cfg(not(windows))]
                    SIGNAL => loop {
                        match self.signals.receive()? {
                            Some(Signal::Interrupt) | Some(Signal::Terminate) => {
                                log::info!("terminating gracefully...");
                                return Ok(());
                            },
                            Some(signal) => {
                                log::info!("ignoring signal {signal:?}");
                            },
                            None => break,
                        }
                    },
                    _ => panic!("what happened here?")
                }
            }
        }
    }

    fn send_event(tx: &UdpSocket, e: Event, addr: SocketAddr) {
        let data: Vec<u8> = (&e).into();
        if let Err(e) = tx.send_to(&data[..], addr) {
            log::error!("{}", e);
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
