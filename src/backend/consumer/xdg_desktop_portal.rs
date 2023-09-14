use crate::consumer::Consumer;

pub struct DesktopPortalConsumer {}

impl DesktopPortalConsumer {
    pub fn new() -> Self { Self {  } }
}

impl Consumer for DesktopPortalConsumer {
    fn consume(&self, _: crate::event::Event, _: crate::client::ClientHandle) {
        log::error!("xdg_desktop_portal backend not yet implemented!");
        todo!()
    }

    fn notify(&mut self, _: crate::client::ClientEvent) {
        todo!()
    }
}
