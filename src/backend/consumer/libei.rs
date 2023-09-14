use crate::consumer::Consumer;

pub struct LibeiConsumer {}

impl LibeiConsumer {
    pub fn new() -> Self { Self {  } }
}

impl Consumer for LibeiConsumer {
    fn consume(&self, _: crate::event::Event, _: crate::client::ClientHandle) {
        log::error!("libei backend not yet implemented!");
        todo!()
    }

    fn notify(&mut self, _: crate::client::ClientEvent) {
        todo!()
    }
}
