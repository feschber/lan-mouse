use anyhow::Result;
use log;

use std::{
    collections::HashMap,
    error::Error,
    net::{SocketAddr, UdpSocket},
    os::fd::AsRawFd,
};

use crate::{client::{ClientManager, ClientHandle}, consumer::Consumer, producer::{EventProducer, EpollProducer}, event::epoll::Epoll};

use super::Event;

pub struct Server {
    listen_addr: SocketAddr,
}

impl Server {
    pub fn new(port: u16) -> Result<Self, Box<dyn Error>> {
        let listen_addr = SocketAddr::new("0.0.0.0".parse()?, port);
        Ok(Server { listen_addr })
    }

    pub fn run(
        &self,
        _client_manager: ClientManager,
        producer: EventProducer,
        consumer: Box<dyn Consumer>,
    ) -> Result<()> {
        let udp_socket = UdpSocket::bind(self.listen_addr)?;
        let rx = udp_socket.try_clone()?;
        let tx = udp_socket;

        match producer {
            EventProducer::Epoll(producer) => {
                #[cfg(windows)]
                panic!("epoll not supported!");
                #[cfg(not(windows))]
                self.epoll_event_loop(rx, tx, producer, consumer)?;
            },
            EventProducer::ThreadProducer(_) => todo!(),
        }
        Ok(())
    }

    fn epoll_event_loop(
        &self,
        rx: UdpSocket,
        tx: UdpSocket,
        mut producer: Box<dyn EpollProducer>,
        consumer: Box<dyn Consumer>,
    ) -> Result<()> {
        let udpfd = rx.as_raw_fd();
        let eventfd = producer.eventfd();
        let epoll = Epoll::new(&[udpfd, eventfd])?;
        let client_for_socket: HashMap<SocketAddr, ClientHandle> = HashMap::new();
        let socket_for_client: HashMap<ClientHandle, SocketAddr> = HashMap::new();
        loop {
            match epoll.wait() {
                fd if fd == udpfd => {
                    match Self::receive_event(&rx) {
                        Ok((event, addr)) => {
                            match client_for_socket.get(&addr) {
                                Some(client_handle) => {
                                    consumer.consume(event, *client_handle);
                                },
                                None => {
                                    log::warn!("ignoring event from client {addr:?}");
                                },
                            }
                        },
                        Err(e) => {
                            log::error!("{e}");
                        },
                    }
                },
                fd if fd == eventfd => {
                    let events = producer.read_events();
                    events.into_iter().for_each(|(c, e)| {
                        if let Some(addr) = socket_for_client.get(&c) {
                            Self::send_event(&tx, e, *addr);
                        } else {
                            log::error!("unknown client: id {c}");
                        }
                    })
                },
                _ => panic!("what happened here?")
            }
        }
    }

    fn send_event(tx: &UdpSocket, e: Event, addr: SocketAddr) {
        let data: Vec<u8> = (&e).into();
        if let Err(e) = tx.send_to(&data[..], addr) {
            log::error!("{}", e);
        }
    }

    fn receive_event(rx: &UdpSocket) -> Result<(Event, SocketAddr), Box<dyn Error>> {
        let mut buf = vec![0u8; 22];
        match rx.recv_from(&mut buf) {
            Ok((_amt, src)) => Ok((Event::try_from(buf)?, src)),
            Err(e) => Err(Box::new(e)),
        }
    }
}
