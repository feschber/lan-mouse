mod imp;

use gtk::glib::{self, Object};
use adw::subclass::prelude::*;

glib::wrapper! {
    pub struct ClientObject(ObjectSubclass<imp::ClientObject>);
}

impl ClientObject {
    pub fn new(hostname: String, port: u32, active: bool, position: String) -> Self {
        Object::builder()
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
    pub hostname: String,
    pub port: u32,
    pub active: bool,
    pub position: String,
}
