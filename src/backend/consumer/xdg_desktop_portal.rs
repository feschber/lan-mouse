use crate::consumer::Consumer;

pub struct DesktopPortalConsumer {}

impl DesktopPortalConsumer {
    pub fn new() -> Self { Self {  } }
}

impl Consumer for DesktopPortalConsumer {
    fn consume(&self, event: crate::event::Event, client_handle: crate::client::ClientHandle) {
        todo!()
    }

    fn notify(&self, client_event: crate::client::ClientEvent) {
        todo!()
    }
}
