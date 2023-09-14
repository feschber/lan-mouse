mod imp;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{glib, gio, NoSelection};
use glib::{clone, Object};

use crate::{gtk::client_object::ClientObject, config::DEFAULT_PORT};

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

    fn clients(&self) -> gio::ListStore {
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
                row.upcast()
            })
        );
    }

    /// workaround for a bug in libadwaita that shows an ugly line beneath
    /// the last element if a placeholder is set.
    fn set_placeholder_visible(&self, visible: bool) {
        let placeholder = self.imp().client_placeholder.get();
        self.imp().client_list.set_placeholder(match visible {
            true => Some(&placeholder),
            false => None,
        });
    }

    fn setup_icon(&self) {
        self.set_icon_name(Some("mouse-icon"));
    }

    fn create_client_row(&self, client_object: &ClientObject) -> ClientRow {
        let row = ClientRow::new();
        row.bind(client_object);
        row
    }

    fn new_client(&self) {
        let client = ClientObject::new(String::from(""), DEFAULT_PORT as u32, true, "left".into());
        self.clients().append(&client);
    }

    fn setup_callbacks(&self) {
        self.imp()
            .add_client_button
            .connect_clicked(clone!(@weak self as window => move |_| {
                window.new_client();
                window.set_placeholder_visible(false);
            }));
    }
}
