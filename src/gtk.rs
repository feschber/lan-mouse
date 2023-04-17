mod window;

use crate::gtk::window::Window;

use gtk::prelude::*;
use std::thread;
use adw::Application;
use gtk::{gio, glib, prelude::ApplicationExt};

pub(crate) fn start() -> thread::JoinHandle<glib::ExitCode> {
    thread::spawn(gtk_main)
}

fn gtk_main() -> glib::ExitCode {
    gio::resources_register_include!("lan-mouse.gresource")
        .expect("Failed to register resources.");

    let app = Application::builder()
        .application_id("de.feschber.lan-mouse")
        .build();

    app.connect_activate(build_ui);

    app.run()
}

fn build_ui(app: &Application) {
    let window = Window::new(app);
    window.present();
}
