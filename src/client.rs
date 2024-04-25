use std::{
    collections::HashSet,
    fmt::Display,
    net::{IpAddr, SocketAddr},
};

use serde::{Deserialize, Serialize};
use slab::Slab;

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
        write!(
            f,
            "{}",
            match self {
                Position::Left => "left",
                Position::Right => "right",
                Position::Top => "top",
                Position::Bottom => "bottom",
            }
        )
    }
}

impl TryFrom<&str> for Position {
    type Error = ();

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "left" => Ok(Position::Left),
            "right" => Ok(Position::Right),
            "top" => Ok(Position::Top),
            "bottom" => Ok(Position::Bottom),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub struct Client {
    /// hostname of this client
    pub hostname: Option<String>,
    /// fix ips, determined by the user
    pub fix_ips: Vec<IpAddr>,
    /// all ip addresses associated with a particular client
    /// e.g. Laptops usually have at least an ethernet and a wifi port
    /// which have different ip addresses
    pub ips: HashSet<IpAddr>,
    /// both active_addr and addrs can be None / empty so port needs to be stored seperately
    pub port: u16,
    /// position of a client on screen
    pub pos: Position,
}

#[derive(Clone, Copy, Debug)]
pub enum ClientEvent {
    Create(ClientHandle, Position),
    Destroy(ClientHandle),
}

pub type ClientHandle = u64;

#[derive(Debug, Clone)]
pub struct ClientState {
    /// information about the client
    pub client: Client,
    /// events should be sent to and received from the client
    pub active: bool,
    /// `active` address of the client, used to send data to.
    /// This should generally be the socket address where data
    /// was last received from.
    pub active_addr: Option<SocketAddr>,
    /// tracks whether or not the client is responding to pings
    pub alive: bool,
    /// keys currently pressed by this client
    pub pressed_keys: HashSet<u32>,
}

pub struct ClientManager {
    clients: Slab<ClientState>,
}

impl Default for ClientManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientManager {
    pub fn new() -> Self {
        let clients = Slab::new();
        Self { clients }
    }

    /// add a new client to this manager
    pub fn add_client(
        &mut self,
        hostname: Option<String>,
        ips: HashSet<IpAddr>,
        port: u16,
        pos: Position,
        active: bool,
    ) -> ClientHandle {
        // store fix ip addresses
        let fix_ips = ips.iter().cloned().collect();

        let client_state = ClientState {
            client: Client {
                hostname,
                fix_ips,
                ips,
                port,
                pos,
            },
            active,
            active_addr: None,
            alive: false,
            pressed_keys: HashSet::new(),
        };

        self.clients.insert(client_state) as ClientHandle
    }

    /// find a client by its address
    pub fn get_client(&self, addr: SocketAddr) -> Option<ClientHandle> {
        // since there shouldn't be more than a handful of clients at any given
        // time this is likely faster than using a HashMap
        self.clients
            .iter()
            .find_map(|(k, c)| {
                if c.active && c.client.ips.contains(&addr.ip()) {
                    Some(k)
                } else {
                    None
                }
            })
            .map(|p| p as ClientHandle)
    }

    pub fn find_client(&self, pos: Position) -> Option<ClientHandle> {
        self.clients
            .iter()
            .find_map(|(k, c)| {
                if c.active && c.client.pos == pos {
                    Some(k)
                } else {
                    None
                }
            })
            .map(|p| p as ClientHandle)
    }

    /// remove a client from the list
    pub fn remove_client(&mut self, client: ClientHandle) -> Option<ClientState> {
        // remove id from occupied ids
        self.clients.try_remove(client as usize)
    }

    // returns an immutable reference to the client state corresponding to `client`
    pub fn get(&self, client: ClientHandle) -> Option<&ClientState> {
        self.clients.get(client as usize)
    }

    /// returns a mutable reference to the client state corresponding to `client`
    pub fn get_mut(&mut self, client: ClientHandle) -> Option<&mut ClientState> {
        self.clients.get_mut(client as usize)
    }

    pub fn get_client_states(&self) -> impl Iterator<Item = (ClientHandle, &ClientState)> {
        self.clients.iter().map(|(k, v)| (k as ClientHandle, v))
    }

    pub fn get_client_states_mut(
        &mut self,
    ) -> impl Iterator<Item = (ClientHandle, &mut ClientState)> {
        self.clients.iter_mut().map(|(k, v)| (k as ClientHandle, v))
    }
}
