mod window;
mod client_object;
mod client_row;

use std::{io::Result, thread::{self, JoinHandle}};

use crate::frontend::gtk::window::Window;

use gtk::{prelude::*, IconTheme, gdk::Display, gio::{SimpleAction, SimpleActionGroup}, glib::clone, CssProvider};
use adw::Application;
use gtk::{gio, glib, prelude::ApplicationExt};

use self::client_object::ClientObject;

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
    app.connect_startup(|_| load_css());
    app.connect_activate(build_ui);

    app.run()
}

fn load_css() {
    let provider = CssProvider::new();
    provider.load_from_resource("de/feschber/LanMouse/style.css");
    gtk::style_context_add_provider_for_display(
    &Display::default().expect("Could not connect to a display."),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn load_icons() {
    let icon_theme = IconTheme::for_display(&Display::default().expect("Could not connect to a display."));
    icon_theme.add_resource_path("/de/feschber/LanMouse/icons");
}

fn build_ui(app: &Application) {
    let window = Window::new(app);
    let action_client_activate = SimpleAction::new(
        "activate-client",
        Some(&i32::static_variant_type()),
    );
    let action_client_delete = SimpleAction::new(
        "delete-client",
        Some(&i32::static_variant_type()),
    );
    action_client_activate.connect_activate(clone!(@weak window => move |_action, param| {
        log::debug!("activate-client");
        let index = param.unwrap()
            .get::<i32>()
            .unwrap();
        let Some(client) = window.clients().item(index as u32) else {
            return;
        };
        let client = client.downcast_ref::<ClientObject>().unwrap();
        window.update_client(client);
    }));
    action_client_delete.connect_activate(clone!(@weak window => move |_action, param| {
        log::debug!("delete-client");
        let index = param.unwrap()
            .get::<i32>()
            .unwrap();
        let Some(client) = window.clients().item(index as u32) else {
            return;
        };
        let client = client.downcast_ref::<ClientObject>().unwrap();
        window.update_client(client);
        window.clients().remove(index as u32);
    }));

    let actions = SimpleActionGroup::new();
    window.insert_action_group("win", Some(&actions));
    actions.add_action(&action_client_activate);
    actions.add_action(&action_client_delete);
    window.present();
}
