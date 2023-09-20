mod imp;

use std::{path::{Path, PathBuf}, env, process, os::unix::net::UnixStream, io::Write};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{glib, gio, NoSelection};
use glib::{clone, Object};

use crate::{frontend::{gtk::client_object::ClientObject, FrontendEvent}, config::DEFAULT_PORT, client::Position};

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
    pub fn set_placeholder_visible(&self, visible: bool) {
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
        let row = ClientRow::new(client_object);
        row.bind(client_object);
        row
    }

    fn new_client(&self) {
        let client = ClientObject::new(String::from(""), DEFAULT_PORT as u32, false, "left".into());
        self.clients().append(&client);
    }

    pub fn update_client(&self, client: &ClientObject) {
        let data = client.get_data();
        let socket_path = self.imp().socket_path.borrow();
        let socket_path = socket_path.as_ref().unwrap().as_path();
        let host_name = data.hostname;
        let position = match data.position.as_str() {
            "left" => Position::Left,
            "right" => Position::Right,
            "top" => Position::Top,
            "bottom" => Position::Bottom,
            _ => {
                log::error!("invalid position: {}", data.position);
                return
            }
        };
        let port = data.port;
        let event = if client.active() {
            FrontendEvent::DelClient(host_name, port as u16)
        } else {
            FrontendEvent::AddClient(host_name, port as u16, position)
        };
        let json = serde_json::to_string(&event).unwrap();
        let Ok(mut stream) = UnixStream::connect(socket_path) else {
            log::error!("Could not connect to lan-mouse-socket @ {socket_path:?}");
            return;
        };
        if let Err(e) = stream.write(json.as_bytes()) {
            log::error!("error sending message: {e}");
        };
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
