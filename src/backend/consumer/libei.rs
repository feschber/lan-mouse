use crate::consumer::Consumer;

pub struct LibeiConsumer {}

impl LibeiConsumer {
    pub fn new() -> Self { Self {  } }
}

impl Consumer for LibeiConsumer {
    fn consume(&self, event: crate::event::Event, client_handle: crate::client::ClientHandle) {
        todo!()
    }

    fn notify(&self, client_event: crate::client::ClientEvent) {
        todo!()
    }
}
