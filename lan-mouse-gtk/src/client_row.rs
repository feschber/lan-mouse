mod imp;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::{self, Object};

use lan_mouse_ipc::{DEFAULT_PORT, Position};

use super::ClientObject;

glib::wrapper! {
    pub struct ClientRow(ObjectSubclass<imp::ClientRow>)
    @extends gtk::ListBoxRow, gtk::Widget, adw::PreferencesRow, adw::ExpanderRow,
    @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl ClientRow {
    pub fn new(client_object: &ClientObject) -> Self {
        let client_row: Self = Object::builder().build();
        client_row
            .imp()
            .client_object
            .borrow_mut()
            .replace(client_object.clone());
        client_row
    }

    pub fn bind(&self, client_object: &ClientObject) {
        let mut bindings = self.imp().bindings.borrow_mut();

        // bind client active to switch state
        let active_binding = client_object
            .bind_property("active", &self.imp().enable_switch.get(), "state")
            .sync_create()
            .build();

        // bind client active to switch position
        let switch_position_binding = client_object
            .bind_property("active", &self.imp().enable_switch.get(), "active")
            .sync_create()
            .build();

        // bind hostname to hostname edit field
        let hostname_binding = client_object
            .bind_property("hostname", &self.imp().hostname.get(), "text")
            .transform_to(|_, v: Option<String>| {
                if let Some(hostname) = v {
                    Some(hostname)
                } else {
                    Some("".to_string())
                }
            })
            .sync_create()
            .build();

        // bind hostname to title
        let title_binding = client_object
            .bind_property("hostname", self, "title")
            .transform_to(|_, v: Option<String>| v.or(Some("<span font_style=\"italic\" font_weight=\"light\" foreground=\"darkgrey\">no hostname!</span>".to_string())))
            .sync_create()
            .build();

        // bind port to port edit field
        let port_binding = client_object
            .bind_property("port", &self.imp().port.get(), "text")
            .transform_to(|_, v: u32| {
                if v == DEFAULT_PORT as u32 {
                    Some("".to_string())
                } else {
                    Some(v.to_string())
                }
            })
            .sync_create()
            .build();

        // bind port to subtitle
        let subtitle_binding = client_object
            .bind_property("port", self, "subtitle")
            .sync_create()
            .build();

        // bind position to selected position
        let position_binding = client_object
            .bind_property("position", &self.imp().position.get(), "selected")
            .transform_to(|_, v: String| match v.as_str() {
                "right" => Some(1u32),
                "top" => Some(2u32),
                "bottom" => Some(3u32),
                _ => Some(0u32),
            })
            .sync_create()
            .build();

        // bind resolving status to spinner visibility
        let resolve_binding = client_object
            .bind_property(
                "resolving",
                &self.imp().dns_loading_indicator.get(),
                "spinning",
            )
            .sync_create()
            .build();

        // bind ips to tooltip-text
        let ip_binding = client_object
            .bind_property("ips", &self.imp().dns_button.get(), "tooltip-text")
            .transform_to(|_, ips: Vec<String>| {
                if ips.is_empty() {
                    Some("no ip addresses associated with this client".into())
                } else {
                    Some(ips.join("\n"))
                }
            })
            .sync_create()
            .build();

        bindings.push(active_binding);
        bindings.push(switch_position_binding);
        bindings.push(hostname_binding);
        bindings.push(title_binding);
        bindings.push(port_binding);
        bindings.push(subtitle_binding);
        bindings.push(position_binding);
        bindings.push(resolve_binding);
        bindings.push(ip_binding);
    }

    pub fn unbind(&self) {
        for binding in self.imp().bindings.borrow_mut().drain(..) {
            binding.unbind();
        }
    }

    pub fn set_active(&self, active: bool) {
        self.imp().set_active(active);
    }

    pub fn set_hostname(&self, hostname: Option<String>) {
        self.imp().set_hostname(hostname);
    }

    pub fn set_port(&self, port: u16) {
        self.imp().set_port(port);
    }

    pub fn set_position(&self, pos: Position) {
        self.imp().set_pos(pos);
    }

    pub fn set_dns_state(&self, resolved: bool) {
        self.imp().set_dns_state(resolved);
    }
}
