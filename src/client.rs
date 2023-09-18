use std::net::SocketAddr;

use serde::{Serialize, Deserialize};

#[derive(Debug, Eq, Hash, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub enum Position {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, Serialize, Deserialize)]
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

pub type ClientHandle = u32;
