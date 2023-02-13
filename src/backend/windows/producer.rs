use std::sync::mpsc::SyncSender;

use crate::{
    client::{Client, ClientHandle},
    event::Event,
    request::Server,
};

pub fn run(_produce_tx: SyncSender<(Event, ClientHandle)>, _server: Server, _clients: Vec<Client>) {
    todo!();
}
