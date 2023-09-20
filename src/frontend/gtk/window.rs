mod imp;

use std::{path::{Path, PathBuf}, env, process};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{glib::{self, Value}, gio, NoSelection};
use glib::{clone, Object};

use crate::{frontend::gtk::client_object::ClientObject, config::DEFAULT_PORT};

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
                row.upcast()
            })
        );
    }

    /// workaround for a bug in libadwaita that shows an ugly line beneath
    /// the last element if a placeholder is set.
    /// https://gitlab.gnome.org/GNOME/gtk/-/merge_requests/6308
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

    fn connect_stream(&self) {
        let xdg_runtime_dir = match env::var("XDG_RUNTIME_DIR") {
            Ok(v) => v,
            Err(e) => {
                log::error!("{e}");
                process::exit(1);
            }
        };
        let socket_path = Path::new(xdg_runtime_dir.as_str())
            .join("lan-mouse-socket.sock");
        self.imp().socket_path.borrow_mut().replace(PathBuf::from(socket_path));
    }
}
