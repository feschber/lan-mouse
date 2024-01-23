mod imp;

use std::io::Write;

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::{clone, Object};
use gtk::{
    gio,
    glib::{self, closure_local},
    NoSelection,
};

use crate::{
    client::{Client, ClientHandle, Position},
    config::DEFAULT_PORT,
    frontend::{gtk::client_object::ClientObject, FrontendEvent},
};

use super::client_row::ClientRow;

glib::wrapper! {
    pub struct Window(ObjectSubclass<imp::Window>)
        @extends adw::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl Window {
    pub(crate) fn new(app: &adw::Application) -> Self {
        Object::builder().property("application", app).build()
    }

    pub fn clients(&self) -> gio::ListStore {
        self.imp()
            .clients
            .borrow()
            .clone()
            .expect("Could not get clients")
    }

    fn setup_clients(&self) {
        let model = gio::ListStore::new::<ClientObject>();
        self.imp().clients.replace(Some(model));

        let selection_model = NoSelection::new(Some(self.clients()));
        self.imp().client_list.bind_model(
            Some(&selection_model),
            clone!(@weak self as window => @default-panic, move |obj| {
                let client_object = obj.downcast_ref().expect("Expected object of type `ClientObject`.");
                let row = window.create_client_row(client_object);
                row.connect_closure("request-update", false, closure_local!(@strong window => move |row: ClientRow, active: bool| {
                    let index = row.index() as u32;
                    let Some(client) = window.clients().item(index) else {
                        return;
                    };
                    let client = client.downcast_ref::<ClientObject>().unwrap();
                    window.request_client_update(client, active);
                }));
                row.connect_closure("request-delete", false, closure_local!(@strong window => move |row: ClientRow| {
                    let index = row.index() as u32;
                    window.request_client_delete(index);
                }));
                row.upcast()
            })
        );
    }

    /// workaround for a bug in libadwaita that shows an ugly line beneath
    /// the last element if a placeholder is set.
    /// https://gitlab.gnome.org/GNOME/gtk/-/merge_requests/6308
    pub fn set_placeholder_visible(&self, visible: bool) {
        let placeholder = self.imp().client_placeholder.get();
        self.imp().client_list.set_placeholder(match visible {
            true => Some(&placeholder),
            false => None,
        });
    }

    fn setup_icon(&self) {
        self.set_icon_name(Some("de.feschber.LanMouse"));
    }

    fn create_client_row(&self, client_object: &ClientObject) -> ClientRow {
        let row = ClientRow::new(client_object);
        row.bind(client_object);
        row
    }

    pub fn new_client(&self, client: Client, active: bool) {
        let client = ClientObject::new(client, active);
        self.clients().append(&client);
        self.set_placeholder_visible(false);
    }

    pub fn client_idx(&self, handle: ClientHandle) -> Option<usize> {
        self.clients().iter::<ClientObject>().position(|c| {
            if let Ok(c) = c {
                c.handle() == handle
            } else {
                false
            }
        })
    }

    pub fn delete_client(&self, handle: ClientHandle) {
        let Some(idx) = self.client_idx(handle) else {
            log::warn!("could not find client with handle {handle}");
            return;
        };

        self.clients().remove(idx as u32);
        if self.clients().n_items() == 0 {
            self.set_placeholder_visible(true);
        }
    }

    pub fn update_client(&self, client: Client) {
        let Some(idx) = self.client_idx(client.handle) else {
            log::warn!("could not find client with handle {}", client.handle);
            return;
        };
        let client_object = self.clients().item(idx as u32).unwrap();
        let client_object: &ClientObject = client_object.downcast_ref().unwrap();
        let data = client_object.get_data();

        /* only change if it actually has changed, otherwise
         * the update signal is triggered */
        if data.hostname != client.hostname {
            client_object.set_hostname(client.hostname.unwrap_or("".into()));
        }
        if data.port != client.port as u32 {
            client_object.set_port(client.port as u32);
        }
        if data.position != client.pos.to_string() {
            client_object.set_position(client.pos.to_string());
        }
    }

    pub fn activate_client(&self, handle: ClientHandle, active: bool) {
        let Some(idx) = self.client_idx(handle) else {
            log::warn!("could not find client with handle {handle}");
            return;
        };
        let client_object = self.clients().item(idx as u32).unwrap();
        let client_object: &ClientObject = client_object.downcast_ref().unwrap();
        let data = client_object.get_data();
        if data.active != active {
            client_object.set_active(active);
            log::debug!("set active to {active}");
        }
    }

    pub fn request_client_create(&self) {
        let event = FrontendEvent::AddClient(None, DEFAULT_PORT, Position::default());
        self.imp().set_port(DEFAULT_PORT);
        self.request(event);
    }

    pub fn request_port_change(&self) {
        let port = self.imp().port_entry.get().text().to_string();
        if let Ok(port) = port.as_str().parse::<u16>() {
            self.request(FrontendEvent::ChangePort(port));
        } else {
            self.request(FrontendEvent::ChangePort(DEFAULT_PORT));
        }
    }

    pub fn request_client_update(&self, client: &ClientObject, active: bool) {
        let data = client.get_data();
        let position = match Position::try_from(data.position.as_str()) {
            Ok(pos) => pos,
            _ => {
                log::error!("invalid position: {}", data.position);
                return;
            }
        };
        let hostname = data.hostname;
        let port = data.port as u16;

        let event = FrontendEvent::UpdateClient(client.handle(), hostname, port, position);
        log::debug!("requesting update: {event:?}");
        self.request(event);

        let event = FrontendEvent::ActivateClient(client.handle(), active);
        log::debug!("requesting activate: {event:?}");
        self.request(event);
    }

    pub fn request_client_delete(&self, idx: u32) {
        if let Some(obj) = self.clients().item(idx) {
            let client_object: &ClientObject = obj
                .downcast_ref()
                .expect("Expected object of type `ClientObject`.");
            let handle = client_object.handle();
            let event = FrontendEvent::DelClient(handle);
            self.request(event);
        }
    }

    fn request(&self, event: FrontendEvent) {
        let json = serde_json::to_string(&event).unwrap();
        log::debug!("requesting {json}");
        let mut stream = self.imp().stream.borrow_mut();
        let stream = stream.as_mut().unwrap();
        let bytes = json.as_bytes();
        let len = bytes.len().to_be_bytes();
        if let Err(e) = stream.write(&len) {
            log::error!("error sending message: {e}");
        };
        if let Err(e) = stream.write(bytes) {
            log::error!("error sending message: {e}");
        };
    }

    pub fn show_toast(&self, msg: &str) {
        let toast = adw::Toast::new(msg);
        let toast_overlay = &self.imp().toast_overlay;
        toast_overlay.add_toast(toast);
    }
}
