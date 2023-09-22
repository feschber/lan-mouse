use std::{process, error::Error};

use env_logger::Env;
use lan_mouse::{
    consumer, producer,
    config::{Config, Frontend::{Cli, Gtk}}, event::server::Server,
    frontend::{FrontendListener, cli},
};

#[cfg(all(unix, feature = "gtk"))]
use lan_mouse::frontend::gtk;

pub fn main() {

    // init logging
    let env = Env::default().filter_or("LAN_MOUSE_LOG_LEVEL", "info");
    env_logger::init_from_env(env);

    if let Err(e) = run() {
        log::error!("{e}");
        process::exit(1);
    }
}

pub fn run() -> Result<(), Box<dyn Error>> {
    // parse config file
    let config = Config::new()?;

    // start producing and consuming events
    let producer = producer::create()?;
    let consumer = consumer::create()?;

    // create frontend communication adapter
    let frontend_adapter = FrontendListener::new()?;

    // start sending and receiving events
    let mut event_server = Server::new(config.port, producer, consumer, frontend_adapter)?;

    // any threads need to be started after event_server sets up signal handling
    match config.frontend {
        #[cfg(all(unix, feature = "gtk"))]
        Gtk => { gtk::start()?; }
        #[cfg(any(not(feature = "gtk"), not(unix)))]
        Gtk => panic!("gtk frontend requested but feature not enabled!"),
        Cli => { cli::start()?; }
    };

    // this currently causes issues, because the clients from
    // the config arent communicated to gtk yet.
    if config.frontend == Gtk {
        log::warn!("clients defined in config currently have no effect with the gtk frontend");
    } else {
        // add clients from config
        config.get_clients().into_iter().for_each(|(c, h, p)| {
            if c.len() == 0 {
                log::warn!("ignoring client {p}: host_name: '{}' with 0 assigned ips!", h.as_deref().unwrap_or(""));
            }
            log::info!("adding client [{}]{} @ {:?}", p, h.as_deref().unwrap_or(""), c);
            event_server.add_client(h, c, p);
        });
    }

    log::info!("Press Ctrl+Alt+Shift+Super to release the mouse");
    // run event loop
    event_server.run()?;

    Ok(())
}
