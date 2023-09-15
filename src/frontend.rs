use std::{os::fd::RawFd, net::SocketAddr};

use ipc_channel::ipc::{IpcReceiver, IpcSender};
use serde::{Serialize, Deserialize};

use crate::client::{Client, Position};

/// cli frontend
pub mod cli;

/// gtk frontend
#[cfg(all(unix, feature = "gtk"))]
pub mod gtk;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum FrontendEvent {
    RequestPortChange(u16),
    RequestClientAdd(SocketAddr, Position),
    RequestClientDelete(Client),
    RequestClientUpdate(Client),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FrontendNotify {
    NotifyClientCreate(Client),
    NotifyError(String),
}

pub trait Frontend {
    fn start(&mut self);
    fn event_channel(&self) -> &IpcReceiver<FrontendEvent>;
    fn notify_channel(&self) -> &IpcSender<FrontendNotify>;
    fn eventfd(&self) -> Option<RawFd>;
    fn read_event(&self) -> u64;
}
