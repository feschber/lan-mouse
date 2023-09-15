use ipc_channel::ipc::{IpcReceiver, IpcSender};

use crate::client::Client;

/// gtk frontend
#[cfg(all(unix, feature = "gtk"))]
pub mod gtk;

#[derive(Clone, Copy)]
pub enum FrontendEvent {
    RequestPortChange(u16),
    RequestClientAdd(Client),
    RequestClientDelete(Client),
    RequestClientUpdate(Client),
}

#[derive(Clone)]
pub enum FrontendNotify {
    NotifyClientCreate(Client),
    NotifyError(String),
}

pub trait Frontend {
    fn event_channel(&self) -> IpcReceiver<FrontendEvent>;
    fn notify_channel(&self) -> IpcSender<FrontendNotify>;
}
