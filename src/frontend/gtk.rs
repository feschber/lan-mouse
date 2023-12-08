mod window;
mod client_object;
mod client_row;

use std::{io::{Read, ErrorKind}, env, process, str};

use crate::{frontend::gtk::window::Window, config::DEFAULT_PORT};

use gtk::{prelude::*, IconTheme, gdk::Display, gio::{SimpleAction, SimpleActionGroup}, glib::{clone, MainContext, Priority}, CssProvider, subclass::prelude::ObjectSubclassIsExt};
use adw::Application;
use gtk::{gio, glib, prelude::ApplicationExt};

use self::client_object::ClientObject;

use super::FrontendNotify;

pub fn run() -> glib::ExitCode {
    log::debug!("running gtk frontend");
    let ret = gtk_main();
    log::debug!("frontend exited");
    ret
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
    let icon_theme = IconTheme::for_display(&Display::default().expect("Could not connect to a display."));
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
    
    let (sender, receiver) = MainContext::channel::<FrontendNotify>(Priority::default());

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
            let json = str::from_utf8(&buf)
                .unwrap();
            match serde_json::from_str(json) {
                Ok(notify) => sender.send(notify).unwrap(),
                Err(e) => log::error!("{e}"),
            }
        } {
            Ok(()) => {},
            Err(e) => log::error!("{e}"),
        }
    });

    let window = Window::new(app);
    window.imp().stream.borrow_mut().replace(tx);
    receiver.attach(None, clone!(@weak window => @default-return glib::ControlFlow::Break,
        move |notify| {
            match notify {
                FrontendNotify::NotifyClientCreate(client, hostname, port, position) => {
                    window.new_client(client, hostname, port, position, false);
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
                FrontendNotify::Enumerate(clients) => {
                    for (client, active) in clients {
                        if window.client_idx(client.handle).is_some() {
                            continue
                        }
                        window.new_client(
                            client.handle,
                            client.hostname,
                            client.addrs
                                .iter()
                                .next()
                                .map(|s| s.port())
                                .unwrap_or(DEFAULT_PORT),
                            client.pos,
                            active,
                        );
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
            glib::ControlFlow::Continue
        }
    ));

    let action_request_client_update = SimpleAction::new(
        "request-client-update",
        Some(&u32::static_variant_type()),
    );

    // remove client
    let action_client_delete = SimpleAction::new(
        "request-client-delete",
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
        let idx = param.unwrap()
            .get::<u32>()
            .unwrap();
        window.request_client_delete(idx);
    }));

    let actions = SimpleActionGroup::new();
    window.insert_action_group("win", Some(&actions));
    actions.add_action(&action_request_client_update);
    actions.add_action(&action_client_delete);
    window.present();
}
