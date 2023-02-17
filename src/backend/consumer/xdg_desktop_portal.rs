use std::sync::mpsc::Receiver;

use crate::{event::Event, client::{ClientHandle, Client}};



pub(crate) fn run(_consume_rx: Receiver<(Event, ClientHandle)>, _clients: Vec<Client>) {
    todo!()
}
