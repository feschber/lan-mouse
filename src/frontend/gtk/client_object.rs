mod imp;

use gtk::glib::{self, Object};
use adw::subclass::prelude::*;

use crate::client::ClientHandle;

glib::wrapper! {
    pub struct ClientObject(ObjectSubclass<imp::ClientObject>);
}

impl ClientObject {
    pub fn new(handle: ClientHandle, hostname: String, port: u32, position: String, active: bool) -> Self {
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
    pub hostname: String,
    pub port: u32,
    pub active: bool,
    pub position: String,
}
