mod window;
mod client_object;
mod client_row;

use std::{io::{Result, Write}, thread::{self, JoinHandle}, os::unix::net::UnixStream};

use crate::{frontend::gtk::window::Window, client::Position};

use gtk::{prelude::*, IconTheme, gdk::Display, gio::{SimpleAction, SimpleActionGroup}, glib::clone};
use adw::{subclass::prelude::*, Application};
use gtk::{gio, glib, prelude::ApplicationExt};

use self::client_object::ClientObject;

use super::FrontendEvent;

pub fn start() -> Result<JoinHandle<glib::ExitCode>> {
    thread::Builder::new()
        .name("gtk-thread".into())
        .spawn(gtk_main)
}

fn gtk_main() -> glib::ExitCode {
    gio::resources_register_include!("lan-mouse.gresource")
        .expect("Failed to register resources.");

    let app = Application::builder()
        .application_id("de.feschber.lan-mouse")
        .build();

    app.connect_startup(|_| load_icons());
    app.connect_activate(build_ui);

    app.run()
}

fn load_icons() {
    let icon_theme = IconTheme::for_display(&Display::default().expect("Could not connect to a display."));
    icon_theme.add_resource_path("/de/feschber/LanMouse/icons");
}

fn build_ui(app: &Application) {
    let window = Window::new(app);
    let action_client_activate = SimpleAction::new(
        "activate-client",
        Some(&bool::static_variant_type()),
    );
    action_client_activate.connect_activate(clone!(@weak window => move |_action, param| {
        let activate = param.unwrap()
            .get::<bool>()
            .unwrap();
        // let Some(client) = window.clients().item(param) else {
        //     return;
        // };
        let Some(client) = window.clients().item(0) else {
            return;
        };
        let client = client.downcast_ref::<ClientObject>().unwrap();
        let data = client.get_data();
        let socket_path = window.imp().socket_path.borrow();
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
        let event = if activate {
            FrontendEvent::AddClient(host_name, port as u16, position)
        } else {
            FrontendEvent::DelClient(host_name, port as u16)
        };
        let json = serde_json::to_string(&event).unwrap();
        let Ok(mut stream) = UnixStream::connect(socket_path) else {
            log::error!("Could not connect to lan-mouse-socket @ {socket_path:?}");
            return;
        };
        if let Err(e) = stream.write(json.as_bytes()) {
            log::error!("error sending message: {e}");
        };
    }));

    let actions = SimpleActionGroup::new();
    window.insert_action_group("win", Some(&actions));
    actions.add_action(&action_client_activate);
    window.present();
}
