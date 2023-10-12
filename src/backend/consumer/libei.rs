use crate::consumer::EventConsumer;

pub struct LibeiConsumer {}

impl LibeiConsumer {
    pub fn new() -> Self { Self {  } }
}

impl EventConsumer for LibeiConsumer {
    fn consume(&mut self, _: crate::event::Event, _: crate::client::ClientHandle) {
        log::error!("libei backend not yet implemented!");
        todo!()
    }

    fn notify(&mut self, _: crate::client::ClientEvent) {
        todo!()
    }
}
