use std::{
    net::{UdpSocket, SocketAddr},
    error::Error,
    sync::{mpsc::{SyncSender, Receiver}, atomic::{AtomicBool, Ordering}, Arc},
    thread::{self, JoinHandle}, collections::HashMap,
};

use crate::client::{ClientHandle, ClientManager};

use super::{Event, Encode, Decode};

pub struct Server {
    listen_addr: SocketAddr,
    sending: Arc<AtomicBool>,
}

impl Server {
    pub fn new(port: u16) -> Self {
        let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), port);
        let sending = Arc::new(AtomicBool::new(false));
        Server { listen_addr, sending }
    }

    pub fn run(self, client_manager: &mut ClientManager, produce_rx: Receiver<(Event, ClientHandle)>, consume_tx: SyncSender<(Event, ClientHandle)>) -> Result<(JoinHandle<()>, JoinHandle<()>), Box<dyn Error>> {
        let udp_socket = UdpSocket::bind(self.listen_addr)?;
        let rx = udp_socket.try_clone().unwrap();
        let tx = udp_socket;

        let sending = self.sending.clone();

        let mut client_for_socket = HashMap::new();
        for client in client_manager.get_clients() {
            println!("{}: {}", client.handle, client.addr);
            client_for_socket.insert(client.addr, client.handle);
        }
        let receiver = thread::Builder::new().name("event receiver".into()).spawn(move || {
            loop {
                let (event, addr) = match Server::receive_event(&rx) {
                    Some(e) => e,
                    None => { continue },
                };

                let client_handle = match client_for_socket.get(&addr) {
                    Some(c) => *c,
                    None => {
                        println!("Allow connection from {:?}? [Y/n]", addr);
                        continue
                    },
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
                        consume_tx.send((event, client_handle)).expect("event consumer unavailable");
                    }
                } else {
                    if let Event::Release() = event {
                        sending.store(false, Ordering::Release);
                    }
                    // we retrieve all events
                    consume_tx.send((event, client_handle)).expect("event consumer unavailable");
                }
            }
        }).unwrap();

        let sending = self.sending.clone();

        let mut socket_for_client = HashMap::new();
        for client in client_manager.get_clients() {
            socket_for_client.insert(client.handle, client.addr);
        }
        let sender = thread::Builder::new().name("event sender".into()).spawn(move || {
            loop {
                let (event, client_handle) = produce_rx.recv().expect("event producer unavailable");
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
        }).unwrap();
        Ok((receiver, sender))
    }

    fn send_event<E: Encode>(tx: &UdpSocket, e: E, addr: SocketAddr) {
        if let Err(e) = tx.send_to(&e.encode(), addr) {
            eprintln!("{}", e);
        }
    }

    fn receive_event(rx: &UdpSocket) -> Option<(Event, SocketAddr)> {
        let mut buf = vec![0u8; 21];
        if let Ok((_amt, src)) = rx.recv_from(&mut buf) {
            Some((Event::decode(buf), src))
        } else {
            None
        }
    }
}
