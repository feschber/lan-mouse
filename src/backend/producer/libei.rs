use std::{error::Error, result::Result};

use crate::producer::EventProducer;

pub struct LibeiProducer {}

impl LibeiProducer {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        Ok(Self {  })
    }
}

impl EventProducer for LibeiProducer {
    fn notify(&mut self, _event: crate::client::ClientEvent) {
    }

    fn release(&mut self) {
    }

    fn get_async_fd(&self) -> std::io::Result<tokio::io::unix::AsyncFd<std::os::fd::RawFd>> {
    }

    fn read_events(&mut self) -> std::vec::Drain<(crate::client::ClientHandle, crate::event::Event)> {
    }
}
