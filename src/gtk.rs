mod window;
mod client_object;
mod client_row;

use crate::gtk::window::Window;

use gtk::{prelude::*, IconTheme, gdk::Display};
use std::thread;
use adw::Application;
use gtk::{gio, glib, prelude::ApplicationExt};

pub fn start() -> thread::JoinHandle<glib::ExitCode> {
    thread::spawn(gtk_main)
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
    window.present();
}
