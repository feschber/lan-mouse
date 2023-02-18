use std::{net::SocketAddr, error::Error, fmt::Display};

use crate::{config, dns};

#[derive(Eq, Hash, PartialEq, Clone, Copy)]
pub enum Position {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy)]
pub struct Client {
    pub addr: SocketAddr,
    pub pos: Position,
    pub handle: ClientHandle,
}

impl Client {
    pub fn handle(&self) -> ClientHandle {
        return self.handle;
    }
}

pub enum ClientEvent {
    Create(Client),
    Destroy(Client),
}

pub struct ClientManager {
    next_id: u32,
    clients: Vec<Client>,
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
    fn add_client(&mut self, client: &config::Client, pos: Position) -> Result<(), Box<dyn Error>> {
        let ip = match client.ip {
            Some(ip) => ip,
            None => match &client.host_name {
                Some(host_name) => dns::resolve(host_name)?,
                None => return Err(Box::new(ClientConfigError{})),
            },
        };
        let addr = SocketAddr::new(ip, client.port.unwrap_or(42069));
        self.register_client(addr, pos);
        Ok(())
    }

    fn new_id(&mut self) -> ClientHandle {
        self.next_id += 1;
        self.next_id
    }

    pub fn new(config: &config::Config) -> Result<Self, Box<dyn Error>> {

        let mut client_manager = ClientManager {
            next_id: 0,
            clients: Vec::new(),
        };

        // add clients from config
        for (client, pos) in config.clients.iter() {
            client_manager.add_client(&client, *pos)?;
        }

        Ok(client_manager)
    }

    pub fn register_client(&mut self, addr: SocketAddr, pos: Position) {
        let handle = self.new_id();
        let client = Client { addr, pos, handle };
        self.clients.push(client);
    }

    pub fn get_clients(&self) -> Vec<Client> {
        self.clients.clone()
    }
}
