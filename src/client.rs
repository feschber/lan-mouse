use std::net::SocketAddr;

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

impl ClientManager {
    fn add_client(&mut self, client: &config::Client, pos: Position) {
        let ip = match client.ip {
            Some(ip) => ip,
            None => match &client.host_name {
                Some(host_name) => match dns::resolve(host_name) {
                    Ok(ip) => ip,
                    Err(e) => panic!("{}", e),
                },
                None => panic!("neither ip nor hostname specified"),
            },
        };
        let addr = SocketAddr::new(ip, client.port.unwrap_or(42069));
        self.register_client(addr, pos);
    }

    fn new_id(&mut self) -> ClientHandle {
        self.next_id += 1;
        self.next_id
    }

    pub fn new(config: &config::Config) -> Self {

        let mut client_manager = ClientManager {
            next_id: 0,
            clients: Vec::new(),
        };

        // add clients from config
        for client in vec![
            &config.client.left,
            &config.client.right,
            &config.client.top,
            &config.client.bottom,
        ] {
            if let Some(client) = client {
                let pos = match client {
                    client if Some(client) == config.client.left.as_ref() => Position::Left,
                    client if Some(client) == config.client.right.as_ref() => Position::Right,
                    client if Some(client) == config.client.top.as_ref() => Position::Top,
                    client if Some(client) == config.client.bottom.as_ref() => Position::Bottom,
                    _ => panic!(),
                };
                client_manager.add_client(client, pos);
            }
        }

        client_manager
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
