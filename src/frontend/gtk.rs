mod client_object;
mod client_row;
mod window;

use std::{
    env,
    io::{ErrorKind, Read},
    process, str,
};

use crate::frontend::gtk::window::Window;

use adw::Application;
use gtk::{
    gdk::Display, glib::clone, prelude::*, subclass::prelude::ObjectSubclassIsExt, CssProvider,
    IconTheme,
};
use gtk::{gio, glib, prelude::ApplicationExt};

use self::client_object::ClientObject;

use super::FrontendNotify;

pub fn run() -> glib::ExitCode {
    log::debug!("running gtk frontend");
    #[cfg(windows)]
    let ret = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024) // https://gitlab.gnome.org/GNOME/gtk/-/commit/52dbb3f372b2c3ea339e879689c1de535ba2c2c3 -> caused crash on windows
        .name("gtk".into())
        .spawn(gtk_main)
        .unwrap()
        .join()
        .unwrap();
    #[cfg(not(windows))]
    let ret = gtk_main();

    if ret == glib::ExitCode::FAILURE {
        log::error!("frontend exited with failure");
    } else {
        log::info!("frontend exited successfully");
    }

    ret
}

fn gtk_main() -> glib::ExitCode {
    gio::resources_register_include!("lan-mouse.gresource").expect("Failed to register resources.");

    let app = Application::builder()
        .application_id("de.feschber.LanMouse")
        .build();

    app.connect_startup(|_| load_icons());
    app.connect_startup(|_| load_css());
    app.connect_activate(build_ui);

    let args: Vec<&'static str> = vec![];
    app.run_with_args(&args)
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
    let display = &Display::default().expect("Could not connect to a display.");
    let icon_theme = IconTheme::for_display(display);
    icon_theme.add_resource_path("/de/feschber/LanMouse/icons");
}

fn build_ui(app: &Application) {
    log::debug!("connecting to lan-mouse-socket");
    let mut rx = match super::wait_for_service() {
        Ok(stream) => stream,
        Err(e) => {
            log::error!("could not connect to lan-mouse-socket: {e}");
            process::exit(1);
        }
    };
    let tx = match rx.try_clone() {
        Ok(sock) => sock,
        Err(e) => {
            log::error!("{e}");
            process::exit(1);
        }
    };
    log::debug!("connected to lan-mouse-socket");

    let (sender, receiver) = async_channel::bounded(10);

    gio::spawn_blocking(move || {
        match loop {
            // read length
            let mut len = [0u8; 8];
            match rx.read_exact(&mut len) {
                Ok(_) => (),
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => break Ok(()),
                Err(e) => break Err(e),
            };
            let len = usize::from_be_bytes(len);

            // read payload
            let mut buf = vec![0u8; len];
            match rx.read_exact(&mut buf) {
                Ok(_) => (),
                Err(e) if e.kind() == ErrorKind::UnexpectedEof => break Ok(()),
                Err(e) => break Err(e),
            };

            // parse json
            let json = str::from_utf8(&buf).unwrap();
            match serde_json::from_str(json) {
                Ok(notify) => sender.send_blocking(notify).unwrap(),
                Err(e) => log::error!("{e}"),
            }
        } {
            Ok(()) => {}
            Err(e) => log::error!("{e}"),
        }
    });

    let window = Window::new(app);
    window.imp().stream.borrow_mut().replace(tx);
    glib::spawn_future_local(clone!(@weak window => async move {
        loop {
            let notify = receiver.recv().await.unwrap();
            match notify {
                FrontendNotify::NotifyClientActivate(handle, active) => {
                    window.activate_client(handle, active);
                }
                FrontendNotify::NotifyClientCreate(client) => {
                    window.new_client(client, false);
                },
                FrontendNotify::NotifyClientUpdate(client) => {
                    window.update_client(client);
                }
                FrontendNotify::NotifyError(e) => {
                    window.show_toast(e.as_str());
                },
                FrontendNotify::NotifyClientDelete(client) => {
                    window.delete_client(client);
                }
                FrontendNotify::Enumerate(clients) => {
                    for (client, active) in clients {
                        if window.client_idx(client.handle).is_some() {
                            window.activate_client(client.handle, active);
                            window.update_client(client);
                        } else {
                            window.new_client(client, active);
                        }
                    }
                },
                FrontendNotify::NotifyPortChange(port, msg) => {
                    match msg {
                        None => window.show_toast(format!("port changed: {port}").as_str()),
                        Some(msg) => window.show_toast(msg.as_str()),
                    }
                    window.imp().set_port(port);
                }
            }
        }
    }));

    window.present();
}
