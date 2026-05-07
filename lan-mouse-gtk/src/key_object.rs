mod imp;

use adw::subclass::prelude::*;
use gtk::glib::{self, Object};

glib::wrapper! {
    pub struct KeyObject(ObjectSubclass<imp::KeyObject>);
}

impl KeyObject {
    pub fn new(desc: String, fp: String, natural_scroll: bool, mouse_sensitivity: f64) -> Self {
        Object::builder()
            .property("description", desc)
            .property("fingerprint", fp)
            .property("natural-scroll", natural_scroll)
            .property("mouse-sensitivity", mouse_sensitivity)
            .build()
    }

    pub fn get_description(&self) -> String {
        self.imp().description.borrow().clone()
    }

    pub fn get_fingerprint(&self) -> String {
        self.imp().fingerprint.borrow().clone()
    }
}
