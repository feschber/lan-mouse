use async_trait::async_trait;
use crate::{consumer::EventConsumer, event::Event, client::{ClientHandle, ClientEvent}};

pub struct DummyConsumer;

impl DummyConsumer {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl EventConsumer for DummyConsumer {
    async fn consume(&mut self, event: Event, client_handle: ClientHandle) {
        log::info!("received event: ({client_handle}) {event}");
    }
    async fn notify(&mut self, client_event: ClientEvent) {
        log::info!("{client_event:?}");
    }
    async fn destroy(&mut self) {}
}
