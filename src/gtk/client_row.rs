mod imp;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::{self, Object};

use super::client_object::ClientObject;

glib::wrapper! {
    pub struct ClientRow(ObjectSubclass<imp::ClientRow>)
    @extends gtk::ListBoxRow, gtk::Widget, adw::PreferencesRow, adw::ExpanderRow,
    @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ClientRow {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientRow {
    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn bind(&self, client_object: &ClientObject) {
        let mut bindings = self.imp().bindings.borrow_mut();

        let hostname_binding = client_object
            .bind_property("hostname", &self.imp().hostname.get(), "text")
            .bidirectional()
            .sync_create()
            .build();

        let title_binding = client_object
            .bind_property("hostname", self, "title")
            .build();

        let port_binding = client_object
            .bind_property("port", &self.imp().port.get(), "text")
            .bidirectional()
            .sync_create()
            .build();

        let subtitle_binding = client_object
            .bind_property("port", self, "subtitle")
            .sync_create()
            .build();


        // let position_binding = client_object
        //     .bind_property("position", &self.imp().position.get(), "selected-item")
        //     .sync_create()
        //     .build();

        bindings.push(hostname_binding);
        bindings.push(title_binding);
        bindings.push(port_binding);
        bindings.push(subtitle_binding);
        // bindings.push(position_binding);
    }

    pub fn unbind(&self) {
        for binding in self.imp().bindings.borrow_mut().drain(..) {
            binding.unbind();
        }
    }
}
