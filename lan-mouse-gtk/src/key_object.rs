mod imp;

use adw::subclass::prelude::*;
use gtk::glib::{self, Object};

glib::wrapper! {
    pub struct KeyObject(ObjectSubclass<imp::KeyObject>);
}

impl KeyObject {
    pub fn new(desc: String, fp: String) -> Self {
        Object::builder()
            .property("description", desc)
            .property("fingerprint", fp)
            .build()
    }

    pub fn get_description(&self) -> String {
        self.imp().description.borrow().clone()
    }

    pub fn get_fingerprint(&self) -> String {
        self.imp().fingerprint.borrow().clone()
    }
}
