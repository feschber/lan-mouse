mod imp;

use adw::subclass::prelude::*;
use gtk::glib::{self, Object};

use crate::client::{Client, ClientHandle};

glib::wrapper! {
    pub struct ClientObject(ObjectSubclass<imp::ClientObject>);
}

impl ClientObject {
    pub fn new(client: Client, active: bool) -> Self {
        Object::builder()
            .property("handle", client.handle)
            .property("hostname", client.hostname)
            .property("port", client.port as u32)
            .property("position", client.pos.to_string())
            .property("active", active)
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
}
