use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::{
    client::{ClientHandle, ClientEvent},
    event::Event,
    producer::EventProducer,
};

pub struct WindowsProducer {
    _tx: Sender<(ClientHandle, Event)>,
    rx: Option<Receiver<(ClientHandle, Event)>>,
}

impl EventProducer for WindowsProducer {
    fn notify(&mut self, _: ClientEvent) { }

    fn release(&mut self) { }
    
    fn get_wait_channel(&mut self) -> Option<mpsc::Receiver<(ClientHandle, Event)>> {
        self.rx.take()
    }
}

impl WindowsProducer {
    pub(crate) fn new() -> Self {
        let (_tx, rx) = mpsc::channel(1);
        let rx = Some(rx);
        Self { _tx, rx }
    }
}
