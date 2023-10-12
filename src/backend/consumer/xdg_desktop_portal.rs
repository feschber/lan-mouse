use crate::consumer::EventConsumer;

pub struct DesktopPortalConsumer {}

impl DesktopPortalConsumer {
    pub fn new() -> Self { Self {  } }
}

impl EventConsumer for DesktopPortalConsumer {
    fn consume(&mut self, _: crate::event::Event, _: crate::client::ClientHandle) {
        log::error!("xdg_desktop_portal backend not yet implemented!");
    }

    fn notify(&mut self, _: crate::client::ClientEvent) {}
}
