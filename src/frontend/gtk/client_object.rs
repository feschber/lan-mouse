mod imp;

use adw::subclass::prelude::*;
use gtk::glib::{self, Object};

use crate::client::ClientHandle;

glib::wrapper! {
    pub struct ClientObject(ObjectSubclass<imp::ClientObject>);
}

impl ClientObject {
    pub fn new(
        handle: ClientHandle,
        hostname: Option<String>,
        port: u32,
        position: String,
        active: bool,
    ) -> Self {
        Object::builder()
            .property("handle", handle)
            .property("hostname", hostname)
            .property("port", port)
            .property("active", active)
            .property("position", position)
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
