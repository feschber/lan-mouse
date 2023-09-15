use std::{net::SocketAddr, error::Error, fmt::Display, sync::{Arc, atomic::{AtomicBool, Ordering, AtomicU32}, RwLock}};

use serde::{Serialize, Deserialize};

use crate::{config::{self, DEFAULT_PORT}, dns};

#[derive(Debug, Eq, Hash, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub enum Position {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Client {
    pub handle: ClientHandle,
    pub addr: SocketAddr,
    pub pos: Position,
}

pub enum ClientEvent {
    Create(Client),
    Destroy(Client),
    Change(Client),
}

pub struct ClientManager {
    next_id: AtomicU32,
    clients: RwLock<Vec<Client>>,
    subscribers: RwLock<Vec<Arc<AtomicBool>>>,
}

pub type ClientHandle = u32;

#[derive(Debug)]
struct ClientConfigError;

impl Display for ClientConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "neither ip nor hostname specified")
    }
}

impl Error for ClientConfigError {}

impl ClientManager {
    #[allow(dead_code)] // TODO
    fn add_client(&self, client: &config::Client, pos: Position) -> Result<(), Box<dyn Error>> {
        let ip = match client.ip {
            Some(ip) => ip,
            None => match &client.host_name {
                Some(host_name) => dns::resolve(host_name)?,
                None => return Err(Box::new(ClientConfigError{})),
            },
        };
        let addr = SocketAddr::new(ip, client.port.unwrap_or(DEFAULT_PORT));
        self.register_client(addr, pos);
        Ok(())
    }

    fn notify(&self) {
        for subscriber in self.subscribers.read().unwrap().iter() {
            subscriber.store(true, Ordering::SeqCst);
        }
    }

    fn new_id(&self) -> ClientHandle {
        let id = self.next_id.load(Ordering::Acquire);
        self.next_id.store(id + 1, Ordering::Release);
        id as ClientHandle
    }

    pub fn new() -> Result<Self, Box<dyn Error>> {

        let client_manager = ClientManager {
            next_id: AtomicU32::new(0),
            clients: RwLock::new(Vec::new()),
            subscribers: RwLock::new(vec![]),
        };

        Ok(client_manager)
    }

    pub fn register_client(&self, addr: SocketAddr, pos: Position) -> Client {
        let handle = self.new_id();
        let client = Client { addr, pos, handle };
        self.clients.write().unwrap().push(client);
        self.notify();
        client
    }

    pub fn get_clients(&self) -> Vec<Client> {
        self.clients.read().unwrap().clone()
    }

    pub fn subscribe(&self, subscriber: Arc<AtomicBool>) {
        self.subscribers.write().unwrap().push(subscriber);
    }
}
