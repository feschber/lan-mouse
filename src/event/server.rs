use anyhow::Result;

use std::{
    collections::HashMap,
    error::Error,
    net::{SocketAddr, UdpSocket},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
};

use crate::{client::ClientManager, ioutils::{ask_confirmation, ask_position}, consumer::Consumer, producer::EventProducer};

use super::Event;

pub struct Server {
    listen_addr: SocketAddr,
    sending: Arc<AtomicBool>,
}

impl Server {
    pub fn new(port: u16) -> Result<Self, Box<dyn Error>> {
        let listen_addr = SocketAddr::new("0.0.0.0".parse()?, port);
        let sending = Arc::new(AtomicBool::new(false));
        Ok(Server {
            listen_addr,
            sending,
        })
    }

    pub fn run(
        &self,
        client_manager: Arc<ClientManager>,
        producer: EventProducer,
        consumer: Box<dyn Consumer>,
    ) -> Result<(JoinHandle<Result<()>>, JoinHandle<Result<()>>), Box<dyn Error>> {
        let udp_socket = UdpSocket::bind(self.listen_addr)?;
        let rx = udp_socket.try_clone()?;
        let tx = udp_socket;

        let sending = self.sending.clone();
        let clients_updated = Arc::new(AtomicBool::new(true));
        client_manager.subscribe(clients_updated.clone());
        let client_manager_clone = client_manager.clone();

        let receiver = thread::Builder::new()
            .name("event receiver".into())
            .spawn(move || {
                let mut client_for_socket = HashMap::new();

                loop {
                    let (event, addr) = match Server::receive_event(&rx) {
                        Ok(e) => e,
                        Err(e) => {
                            eprintln!("{}", e);
                            continue;
                        }
                    };

                    if let Ok(_) = clients_updated.compare_exchange(
                        true,
                        false,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    ) {
                        clients_updated.store(false, Ordering::SeqCst);
                        client_for_socket.clear();
                        println!("updating clients: ");
                        for client in client_manager_clone.get_clients() {
                            println!("{}: {}", client.handle, client.addr);
                            client_for_socket.insert(client.addr, client.handle);
                        }
                    }

                    let client_handle = match client_for_socket.get(&addr) {
                        Some(c) => *c,
                        None => {
                            eprint!("Allow connection from {:?}? ", addr);
                            if ask_confirmation(false)? {
                                client_manager_clone.register_client(addr, ask_position()?);
                            } else {
                                eprintln!("rejecting client: {:?}?", addr);
                            }
                            continue;
                        }
                    };

                    // There is a race condition between loading this
                    // value and handling the event:
                    // In the meantime a event could be produced, which
                    // should theoretically disable receiving of events.
                    //
                    // This is however not a huge problem, as some
                    // events that make it through are not a large problem
                    if sending.load(Ordering::Acquire) {
                        // ignore received events when in sending state
                        // if release event is received, switch state to receiving
                        if let Event::Release() = event {
                            sending.store(false, Ordering::Release);
                            consumer.consume(event, client_handle);
                        }
                    } else {
                        // we received an event -> set state to receiving
                        if let Event::Release() = event {
                            sending.store(false, Ordering::Release);
                        }
                        consumer.consume(event, client_handle);
                    }
                }
            })?;

        let sending = self.sending.clone();

        let mut socket_for_client = HashMap::new();
        for client in client_manager.get_clients() {
            socket_for_client.insert(client.handle, client.addr);
        }
        let sender = thread::Builder::new()
            .name("event sender".into())
            .spawn(move || {
                loop {
                    let (event, client_handle) =
                        produce_rx.recv().expect("event producer unavailable");
                    let addr = match socket_for_client.get(&client_handle) {
                        Some(addr) => addr,
                        None => continue,
                    };

                    if sending.load(Ordering::Acquire) {
                        Server::send_event(&tx, event, *addr);
                    } else {
                        // only accept enter event
                        if let Event::Release() = event {
                            // set state to sending, to ignore incoming events
                            // and enable sending of events
                            sending.store(true, Ordering::Release);
                            Server::send_event(&tx, event, *addr);
                        }
                    }
                }
            })?;
        Ok((receiver, sender))
    }

    fn send_event(tx: &UdpSocket, e: Event, addr: SocketAddr) {
        let data: Vec<u8> = (&e).into();
        if let Err(e) = tx.send_to(&data[..], addr) {
            eprintln!("{}", e);
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
