mod imp;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::{self, Object};

use crate::config::DEFAULT_PORT;

use super::ClientObject;

glib::wrapper! {
    pub struct ClientRow(ObjectSubclass<imp::ClientRow>)
    @extends gtk::ListBoxRow, gtk::Widget, adw::PreferencesRow, adw::ExpanderRow,
    @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl ClientRow {
    pub fn new(_client_object: &ClientObject) -> Self {
        Object::builder().build()
    }

    pub fn bind(&self, client_object: &ClientObject) {
        let mut bindings = self.imp().bindings.borrow_mut();

        let active_binding = client_object
            .bind_property("active", &self.imp().enable_switch.get(), "state")
            .bidirectional()
            .sync_create()
            .build();

        let switch_position_binding = client_object
            .bind_property("active", &self.imp().enable_switch.get(), "active")
            .bidirectional()
            .sync_create()
            .build();

        let hostname_binding = client_object
            .bind_property("hostname", &self.imp().hostname.get(), "text")
            .transform_to(|_, v: Option<String>| {
                if let Some(hostname) = v {
                    Some(hostname)
                } else {
                    Some("".to_string())
                }
            })
            .transform_from(|_, v: String| {
                if v.as_str().trim() == "" {
                    Some(None)
                } else {
                    Some(Some(v))
                }
            })
            .bidirectional()
            .sync_create()
            .build();

        let title_binding = client_object
            .bind_property("hostname", self, "title")
            .transform_to(|_, v: Option<String>| {
                if let Some(hostname) = v {
                    Some(hostname)
                } else {
                    Some("<span font_style=\"italic\" font_weight=\"light\" foreground=\"darkgrey\">no hostname!</span>".to_string())
                }
            })
            .sync_create()
            .build();

        let port_binding = client_object
            .bind_property("port", &self.imp().port.get(), "text")
            .transform_from(|_, v: String| {
                if v.is_empty() {
                    Some(DEFAULT_PORT as u32)
                } else {
                    Some(v.parse::<u16>().unwrap_or(DEFAULT_PORT) as u32)
                }
            })
            .transform_to(|_, v: u32| {
                if v == 4242 {
                    Some("".to_string())
                } else {
                    Some(v.to_string())
                }
            })
            .bidirectional()
            .sync_create()
            .build();

        let subtitle_binding = client_object
            .bind_property("port", self, "subtitle")
            .sync_create()
            .build();

        let position_binding = client_object
            .bind_property("position", &self.imp().position.get(), "selected")
            .transform_from(|_, v: u32| match v {
                1 => Some("right"),
                2 => Some("top"),
                3 => Some("bottom"),
                _ => Some("left"),
            })
            .transform_to(|_, v: String| match v.as_str() {
                "right" => Some(1),
                "top" => Some(2u32),
                "bottom" => Some(3u32),
                _ => Some(0u32),
            })
            .bidirectional()
            .sync_create()
            .build();

        bindings.push(active_binding);
        bindings.push(switch_position_binding);
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
