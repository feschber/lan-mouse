mod imp;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::{self, Object};

use super::KeyObject;

glib::wrapper! {
    pub struct KeyRow(ObjectSubclass<imp::KeyRow>)
    @extends gtk::ListBoxRow, gtk::Widget, adw::PreferencesRow, adw::ActionRow, adw::ExpanderRow,
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

        // Push initial widget state from KeyObject without firing
        // the user-change signals (no ping-pong on bind).
        let imp = self.imp();
        let switch = &imp.natural_scroll_switch;
        let switch_handler = imp.natural_scroll_handler.borrow();
        if let Some(id) = switch_handler.as_ref() {
            switch.block_signal(id);
        }
        switch.set_active(key_object.natural_scroll());
        switch.set_state(key_object.natural_scroll());
        if let Some(id) = switch_handler.as_ref() {
            switch.unblock_signal(id);
        }

        let spin = &imp.sensitivity_spin;
        let spin_handler = imp.sensitivity_handler.borrow();
        if let Some(id) = spin_handler.as_ref() {
            spin.block_signal(id);
        }
        spin.set_value(key_object.mouse_sensitivity());
        if let Some(id) = spin_handler.as_ref() {
            spin.unblock_signal(id);
        }
    }

    pub fn unbind(&self) {
        for binding in self.imp().bindings.borrow_mut().drain(..) {
            binding.unbind();
        }
    }
}
