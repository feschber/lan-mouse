mod imp;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::{self, Object};

use crate::config::DEFAULT_PORT;

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
            .transform_from(|_, v: String| {
                if v == "" { Some("hostname".into()) } else { Some(v) }
            })
            .bidirectional()
            .sync_create()
            .build();

        let title_binding = client_object
            .bind_property("hostname", self, "title")
            .build();

        let port_binding = client_object
            .bind_property("port", &self.imp().port.get(), "text")
            .transform_from(|_, v: String| {
                if v == "" {
                    Some(4242)
                } else {
                    Some(v.parse::<u16>().unwrap_or(DEFAULT_PORT) as u32)
                }
            })
            .bidirectional()
            .build();

        let subtitle_binding = client_object
            .bind_property("port", self, "subtitle")
            .sync_create()
            .build();


        let position_binding = client_object
            .bind_property("position", &self.imp().position.get(), "selected")
            .transform_from(|_, v: u32| {
                match v {
                    1 => Some("right"),
                    2 => Some("top"),
                    3 => Some("bottom"),
                    _ => Some("left"),
                }
            })
            .transform_to(|_, v: String| {
                match v.as_str() {
                    "right" => Some(1),
                    "top" => Some(2u32),
                    "bottom" => Some(3u32),
                    _ => Some(0u32),
                }
            })
            .bidirectional()
            .sync_create()
            .build();

        bindings.push(hostname_binding);
        bindings.push(title_binding);
        bindings.push(port_binding);
        bindings.push(subtitle_binding);
        bindings.push(position_binding);
    }

    pub fn unbind(&self) {
        for binding in self.imp().bindings.borrow_mut().drain(..) {
            binding.unbind();
        }
    }
}
