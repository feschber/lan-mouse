use std::{net::{SocketAddr, IpAddr}, collections::HashSet, fmt::Display, time::Instant};

use serde::{Serialize, Deserialize};

#[derive(Debug, Eq, Hash, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub enum Position {
    Left,
    Right,
    Top,
    Bottom,
}

impl Default for Position {
    fn default() -> Self {
        Self::Left
    }
}

impl Position {
    pub fn opposite(&self) -> Self {
        match self {
            Position::Left => Self::Right,
            Position::Right => Self::Left,
            Position::Top => Self::Bottom,
            Position::Bottom => Self::Top,
        }
    }
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
    /// hostname of this client
    pub hostname: Option<String>,
    /// unique handle to refer to the client.
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
    /// both active_addr and addrs can be None / empty so port needs to be stored seperately
    pub port: u16,
    /// position of a client on screen
    pub pos: Position,
}

pub enum ClientEvent {
    Create(ClientHandle, Position),
    Destroy(ClientHandle),
}

pub type ClientHandle = u32;

#[derive(Debug, Clone)]
pub struct ClientState {
    pub client: Client,
    pub active: bool,
    pub last_ping: Option<Instant>,
    pub last_seen: Option<Instant>,
    pub last_replied: Option<Instant>,
}

pub struct ClientManager {
    clients: Vec<Option<ClientState>>, // HashMap likely not beneficial
    client_id_in_use: Vec<ClientHandle>, // HashSet likely not beneficial
}

impl ClientManager {
    pub fn new() -> Self {
        Self {
            clients: vec![],
            client_id_in_use: Vec::new(),
        }
    }

    /// add a new client to this manager
    pub fn add_client(
        &mut self,
        hostname: Option<String>,
        addrs: HashSet<IpAddr>,
        port: u16,
        pos: Position,
    ) -> ClientHandle {
        // get a new client_handle
        let handle = self.free_id();

        // we dont know, which IP is initially active
        let active_addr = None;

        // map ip addresses to socket addresses
        let addrs = HashSet::from_iter(
            addrs
                .into_iter()
                .map(|ip| SocketAddr::new(ip, port))
        );

        // store the client
        let client = Client { hostname, handle, active_addr, addrs, port, pos };

        // client was never seen, nor pinged
        let client_state = ClientState {
            client,
            last_ping: None,
            last_seen: None,
            last_replied: None,
            active: false,
        };

        self.clients.push(Some(client_state));
        handle
    }

    /// find a client by its address
    pub fn get_client(&self, addr: SocketAddr) -> Option<ClientHandle> {
        // since there shouldn't be more than a handful of clients at any given
        // time this is likely faster than using a HashMap
        self.clients
            .iter()
            .filter_map(|c| c.as_ref())
            .position(|c| c.client.addrs.contains(&addr))
            .map(|p| p as ClientHandle)
    }

    /// remove a client from the list
    pub fn remove_client(&mut self, client: ClientHandle) -> Option<ClientState> {
        // remove id from occupied ids
        if let Some(idx) = self.client_id_in_use.iter().position(|c| *c == client) {
            self.client_id_in_use.remove(idx as usize);
        }
        // remove client_state from the list
        self.clients.get_mut(client as usize)?.take()
    }

    /// get a free slot in the client list
    fn free_id(&mut self) -> ClientHandle {
        for i in 0..u32::MAX {
            if !self.client_id_in_use.contains(&i) {
                // add the id to the occupied list and return it
                self.client_id_in_use.push(i);
                return i;
            }
        }
        panic!("Out of client ids");
    }

    // returns an immutable reference to the client state corresponding to `client`
    pub fn get<'a>(&'a self, client: ClientHandle) -> Option<&'a ClientState> {
        self.clients.get(client as usize)?.as_ref()
    }

    /// returns a mutable reference to the client state corresponding to `client`
    pub fn get_mut<'a>(&'a mut self, client: ClientHandle) -> Option<&'a mut ClientState> {
        self.clients.get_mut(client as usize)?.as_mut()
    }

    pub fn enumerate(&self) -> Vec<(Client, bool)> {
        self.clients
            .iter()
            .filter_map(|s|s.as_ref())
            .map(|s| (s.client.clone(), s.active))
            .collect()
    }
}
