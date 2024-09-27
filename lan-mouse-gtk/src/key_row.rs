mod imp;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::{self, Object};

use super::KeyObject;

glib::wrapper! {
    pub struct KeyRow(ObjectSubclass<imp::KeyRow>)
    @extends gtk::ListBoxRow, gtk::Widget, adw::PreferencesRow, adw::ExpanderRow,
    @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl KeyRow {
    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn bind(&self, key_object: &KeyObject) {
        let mut bindings = self.imp().bindings.borrow_mut();

        let title_binding = key_object
            .bind_property("fingerprint", self, "title")
            .sync_create()
            .build();

        bindings.push(title_binding);
    }

    pub fn unbind(&self) {
        for binding in self.imp().bindings.borrow_mut().drain(..) {
            binding.unbind();
        }
    }
}
