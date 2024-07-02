use crate::event::Event;
use async_trait::async_trait;

use super::{EmulationHandle, InputEmulation};

#[derive(Default)]
pub struct DummyEmulation;

impl DummyEmulation {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl InputEmulation for DummyEmulation {
    async fn consume(&mut self, event: Event, client_handle: EmulationHandle) {
        log::info!("received event: ({client_handle}) {event}");
    }
    async fn create(&mut self, _: EmulationHandle) {}
    async fn destroy(&mut self, _: EmulationHandle) {}
}
