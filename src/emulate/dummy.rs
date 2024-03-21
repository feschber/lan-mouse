use crate::{
    client::{ClientEvent, ClientHandle},
    emulate::InputEmulation,
    event::Event,
};
use async_trait::async_trait;

#[derive(Default)]
pub struct DummyEmulation;

impl DummyEmulation {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl InputEmulation for DummyEmulation {
    async fn consume(&mut self, event: Event, client_handle: ClientHandle) {
        log::info!("received event: ({client_handle}) {event}");
    }
    async fn notify(&mut self, client_event: ClientEvent) {
        log::info!("{client_event:?}");
    }
    async fn destroy(&mut self) {}
}
