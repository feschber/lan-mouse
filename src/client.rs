use std::{net::SocketAddr, collections::HashSet, fmt::Display};

use serde::{Serialize, Deserialize};

#[derive(Debug, Eq, Hash, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub enum Position {
    Left,
    Right,
    Top,
    Bottom,
}
impl Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", match self {
            Position::Left => "left",
            Position::Right => "right",
            Position::Top => "top",
            Position::Bottom => "bottom",
        })
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub struct Client {
    /// handle to refer to the client.
    /// This way any event consumer / producer backend does not
    /// need to know anything about a client other than its handle.
    pub handle: ClientHandle,
    /// `active` address of the client, used to send data to.
    /// This should generally be the socket address where data
    /// was last received from.
    pub active_addr: Option<SocketAddr>,
    /// all socket addresses associated with a particular client
    /// e.g. Laptops usually have at least an ethernet and a wifi port
    /// which have different ip addresses
    pub addrs: HashSet<SocketAddr>,
    /// position of a client on screen
    pub pos: Position,
}

pub enum ClientEvent {
    Create(ClientHandle, Position),
    Destroy(ClientHandle),
    UpdatePos(ClientHandle, Position),
    AddAddr(ClientHandle, SocketAddr),
    RemoveAddr(ClientHandle, SocketAddr),
}

pub type ClientHandle = u32;

pub struct ClientManager {
    /// probably not beneficial to use a hashmap here
    clients: Vec<Client>,
    next_client_id: u32,
}

impl ClientManager {
    pub fn new() -> Self {
        Self {
            clients: vec![],
            next_client_id: 0,
        }
    }

    /// add a new client to this manager
    pub fn add_client(&mut self, addrs: HashSet<SocketAddr>, pos: Position) -> ClientHandle {
        let handle = self.next_id();
        // we dont know, which IP is initially active
        let active_addr = None;

        // store the client
        let client = Client { handle, active_addr, addrs, pos };
        self.clients.push(client);

        handle
    }

    /// add a socket address to the given client
    pub fn add_addr(&mut self, client: ClientHandle, addr: SocketAddr) {
        if let Some(client) = self.get_mut(client) {
            client.addrs.insert(addr);
        }
    }

    /// remove socket address from the given client
    pub fn remove_addr(&mut self, client: ClientHandle, addr: SocketAddr) {
        if let Some(client) = self.get_mut(client) {
            client.addrs.remove(&addr);
        }
    }

    pub fn set_default_addr(&mut self, client: ClientHandle, addr: SocketAddr) {
        if let Some(client) = self.get_mut(client) {
            client.active_addr = Some(addr)
        }
    }

    /// update the position of a client
    pub fn update_pos(&mut self, client: ClientHandle, pos: Position) {
        if let Some(client) = self.get_mut(client) {
            client.pos = pos;
        }
    }

    pub fn get_active_addr(&self, client: ClientHandle) -> Option<SocketAddr> {
        self.get(client)?.active_addr
    }

    pub fn get_addrs(&self, client: ClientHandle) -> Option<Vec<SocketAddr>> {
        Some(self.get(client)?.addrs.iter().cloned().collect())
    }

    pub fn get_client(&self, addr: SocketAddr) -> Option<ClientHandle> {
        self.clients
            .iter()
            .find(|c| c.addrs.contains(&addr))
            .map(|c| c.handle)
    }

    fn next_id(&mut self) -> ClientHandle {
        let handle = self.next_client_id;
        self.next_client_id += 1;
        handle
    }

    fn get<'a>(&'a self, client: ClientHandle) -> Option<&'a Client> {
        self.clients
            .iter()
            .find(|c| c.handle == client)
    }

    fn get_mut<'a>(&'a mut self, client: ClientHandle) -> Option<&'a mut Client> {
        self.clients
            .iter_mut()
            .find(|c| c.handle == client)
    }
}
