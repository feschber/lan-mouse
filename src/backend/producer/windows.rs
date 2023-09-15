use std::sync::mpsc::{SyncSender, Receiver, sync_channel};

use crate::{
    client::{Client, ClientHandle, ClientEvent},
    event::Event,
    request::Server, producer::ThreadProducer,
};

pub struct WindowsProducer {
    rx: Receiver<(ClientHandle, Event)>,
    _tx: SyncSender<(ClientHandle, Event)>,
}

impl ThreadProducer for WindowsProducer {
    fn notify(&self, _: ClientEvent) { }

    fn produce(&self) -> (ClientHandle, Event) {
        todo!();
    }

    fn stop(&self) { }
}

impl WindowsProducer {
    pub(crate) fn new() -> Self {
        let (_tx, rx) = sync_channel(128);
        Self { rx, _tx }
    }
}

pub fn run(_produce_tx: SyncSender<(Event, ClientHandle)>, _server: Server, _clients: Vec<Client>) {
    todo!();
}
