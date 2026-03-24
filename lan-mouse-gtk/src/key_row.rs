mod imp;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::{self, Object};

use super::KeyObject;

glib::wrapper! {
    pub struct KeyRow(ObjectSubclass<imp::KeyRow>)
    @extends gtk::ListBoxRow, gtk::Widget, adw::PreferencesRow, adw::ActionRow,
    @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for KeyRow {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyRow {
    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn bind(&self, key_object: &KeyObject) {
        let mut bindings = self.imp().bindings.borrow_mut();

        let title_binding = key_object
            .bind_property("description", self, "title")
            .sync_create()
            .build();

        let subtitle_binding = key_object
            .bind_property("fingerprint", self, "subtitle")
            .sync_create()
            .build();

        bindings.push(title_binding);
        bindings.push(subtitle_binding);
    }

    pub fn unbind(&self) {
        for binding in self.imp().bindings.borrow_mut().drain(..) {
            binding.unbind();
        }
    }
}
