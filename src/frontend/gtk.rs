mod window;
mod client_object;
mod client_row;

use std::{io::{Result, Read}, thread::{self, JoinHandle}, env, process, path::Path, os::unix::net::UnixStream, str, cell::RefCell};

use crate::frontend::gtk::window::Window;

use gtk::{prelude::*, IconTheme, gdk::Display, gio::{SimpleAction, SimpleActionGroup}, glib::{clone, MainContext, Priority}, CssProvider, subclass::prelude::ObjectSubclassIsExt};
use adw::Application;
use gtk::{gio, glib, prelude::ApplicationExt};

use self::client_object::ClientObject;

use super::FrontendNotify;

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
    let xdg_runtime_dir = match env::var("XDG_RUNTIME_DIR") {
        Ok(v) => v,
        Err(e) => {
            log::error!("{e}");
            process::exit(1);
        }
    };
    let socket_path = Path::new(xdg_runtime_dir.as_str())
        .join("lan-mouse-socket.sock");
    let Ok(mut rx) = UnixStream::connect(&socket_path) else {
        log::error!("Could not connect to lan-mouse-socket @ {socket_path:?}");
        process::exit(1);
    };
    let tx = match rx.try_clone() {
        Ok(sock) => sock,
        Err(e) => {
            log::error!("{e}");
            process::exit(1);
        }
    };
    
    let (sender, receiver) = MainContext::channel::<FrontendNotify>(Priority::default());

    gio::spawn_blocking(move || {
        loop {
            let mut buf = [0u8; 256];
            if let Ok(_) = rx.read(&mut buf) {
                let json = str::from_utf8(&buf)
                    .unwrap()
                    .trim_end_matches(char::from(0)); // remove trailing 0-bytes
                match serde_json::from_str(json) {
                    Ok(notify) => sender.send(notify).unwrap(),
                    Err(e) => log::error!("{e}"),
                }
            };
        }
    });

    let window = Window::new(app);
    window.imp().stream.borrow_mut().replace(tx);
    receiver.attach(None, clone!(@weak window => @default-return glib::ControlFlow::Break,
        move |notify| {
            match notify {
                FrontendNotify::NotifyClientCreate(client, hostname, port, position) => {
                    window.new_client(client, hostname, port, position);
                },
                FrontendNotify::NotifyClientUpdate(client, hostname, port, position) => {
                    log::info!("client updated: {client}, {}:{port}, {position}", hostname.unwrap_or("".to_string()));
                }
                FrontendNotify::NotifyError(e) => {
                    // TODO
                    log::error!("{e}");
                },
                FrontendNotify::NotifyClientDelete(client) => {
                    window.delete_client(client);
                }
            }
            glib::ControlFlow::Continue
        }
    ));

    let action_request_client_update = SimpleAction::new(
        "request-client-update",
        Some(&u32::static_variant_type()),
    );

    // remove client
    let action_client_delete = SimpleAction::new(
        "delete-client",
        Some(&u32::static_variant_type()),
    );

    // update client state
    action_request_client_update.connect_activate(clone!(@weak window => move |_action, param| {
        log::debug!("request-client-update");
        let index = param.unwrap()
            .get::<u32>()
            .unwrap();
        let Some(client) = window.clients().item(index as u32) else {
            return;
        };
        let client = client.downcast_ref::<ClientObject>().unwrap();
        window.request_client_update(client);
    }));

    action_client_delete.connect_activate(clone!(@weak window => move |_action, param| {
        log::debug!("delete-client");
        let handle = param.unwrap()
            .get::<u32>()
            .unwrap();
        window.delete_client(handle);
    }));

    let actions = SimpleActionGroup::new();
    window.insert_action_group("win", Some(&actions));
    actions.add_action(&action_request_client_update);
    actions.add_action(&action_client_delete);
    window.present();
}
