use async_trait::async_trait;
use crate::client::{ClientEvent, ClientHandle};
use crate::consumer::EventConsumer;
use crate::event::Event;

pub struct MacOSConsumer;

impl MacOSConsumer {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl EventConsumer for MacOSConsumer {
    async fn consume(&mut self, _event: Event, _client_handle: ClientHandle) { }

    async fn notify(&mut self, _client_event: ClientEvent) { }

    async fn destroy(&mut self) { }
}