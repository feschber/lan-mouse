use std::sync::mpsc::SyncSender;

use crate::event::Event;
use crate::request::Server;
use crate::client::Client;


pub fn run(_produce_tx: SyncSender<(Event, u32)>, _request_server: Server, _clients: Vec<Client>) {
    todo!()
}
