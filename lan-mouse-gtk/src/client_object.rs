mod imp;

use adw::subclass::prelude::*;
use gtk::glib::{self, Object};

use lan_mouse_ipc::{ClientConfig, ClientHandle, ClientState};

glib::wrapper! {
    pub struct ClientObject(ObjectSubclass<imp::ClientObject>);
}

impl ClientObject {
    pub fn new(handle: ClientHandle, client: ClientConfig, state: ClientState) -> Self {
        Object::builder()
            .property("handle", handle)
            .property("hostname", client.hostname)
            .property("port", client.port as u32)
            .property("position", client.pos.to_string())
            .property("active", state.active)
            .property(
                "ips",
                state
                    .ips
                    .iter()
                    .map(|ip| ip.to_string())
                    .collect::<Vec<_>>(),
            )
            .property("resolving", state.resolving)
            .build()
    }

    pub fn get_data(&self) -> ClientData {
        self.imp().data.borrow().clone()
    }
}

#[derive(Default, Clone)]
pub struct ClientData {
    pub handle: ClientHandle,
    pub hostname: Option<String>,
    pub port: u32,
    pub active: bool,
    pub position: String,
    pub resolving: bool,
    pub ips: Vec<String>,
}
