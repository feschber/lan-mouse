use std::{
    collections::HashSet,
    error::Error,
    fmt::Display,
    net::{IpAddr, SocketAddr},
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use slab::Slab;

use crate::config::DEFAULT_PORT;

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

#[derive(Debug)]
pub struct PositionParseError {
    string: String,
}

impl Display for PositionParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not a valid position: {}", self.string)
    }
}

impl Error for PositionParseError {}

impl FromStr for Position {
    type Err = PositionParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "top" => Ok(Self::Top),
            "bottom" => Ok(Self::Bottom),
            _ => Err(PositionParseError { string: s.into() }),
        }
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
pub struct ClientConfig {
    /// hostname of this client
    pub hostname: Option<String>,
    /// fix ips, determined by the user
    pub fix_ips: Vec<IpAddr>,
    /// both active_addr and addrs can be None / empty so port needs to be stored seperately
    pub port: u16,
    /// position of a client on screen
    pub pos: Position,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            hostname: Default::default(),
            fix_ips: Default::default(),
            pos: Default::default(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ClientEvent {
    Create(ClientHandle, Position),
    Destroy(ClientHandle),
}

pub type ClientHandle = u64;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ClientState {
    /// events should be sent to and received from the client
    pub active: bool,
    /// `active` address of the client, used to send data to.
    /// This should generally be the socket address where data
    /// was last received from.
    pub active_addr: Option<SocketAddr>,
    /// tracks whether or not the client is responding to pings
    pub alive: bool,
    /// all ip addresses associated with a particular client
    /// e.g. Laptops usually have at least an ethernet and a wifi port
    /// which have different ip addresses
    pub ips: HashSet<IpAddr>,
    /// keys currently pressed by this client
    pub pressed_keys: HashSet<u32>,
}

pub struct ClientManager {
    clients: Slab<(ClientConfig, ClientState)>,
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
    pub fn add_client(&mut self) -> ClientHandle {
        let client_config = Default::default();
        let client_state = Default::default();
        self.clients.insert((client_config, client_state)) as ClientHandle
    }

    /// find a client by its address
    pub fn get_client(&self, addr: SocketAddr) -> Option<ClientHandle> {
        // since there shouldn't be more than a handful of clients at any given
        // time this is likely faster than using a HashMap
        self.clients
            .iter()
            .find_map(|(k, (_, s))| {
                if s.active && s.ips.contains(&addr.ip()) {
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
            .find_map(|(k, (c, s))| {
                if s.active && c.pos == pos {
                    Some(k)
                } else {
                    None
                }
            })
            .map(|p| p as ClientHandle)
    }

    /// remove a client from the list
    pub fn remove_client(&mut self, client: ClientHandle) -> Option<(ClientConfig, ClientState)> {
        // remove id from occupied ids
        self.clients.try_remove(client as usize)
    }

    // returns an immutable reference to the client state corresponding to `client`
    pub fn get(&self, handle: ClientHandle) -> Option<&(ClientConfig, ClientState)> {
        self.clients.get(handle as usize)
    }

    /// returns a mutable reference to the client state corresponding to `client`
    pub fn get_mut(&mut self, handle: ClientHandle) -> Option<&mut (ClientConfig, ClientState)> {
        self.clients.get_mut(handle as usize)
    }

    pub fn get_client_states(
        &self,
    ) -> impl Iterator<Item = (ClientHandle, &(ClientConfig, ClientState))> {
        self.clients.iter().map(|(k, v)| (k as ClientHandle, v))
    }

    pub fn get_client_states_mut(
        &mut self,
    ) -> impl Iterator<Item = (ClientHandle, &mut (ClientConfig, ClientState))> {
        self.clients.iter_mut().map(|(k, v)| (k as ClientHandle, v))
    }
}
