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

    // add clients from config
    config.get_clients().into_iter().for_each(|(c, h, port, p)| {
        event_server.add_client(h, c, port, p);
    });

    log::info!("Press Ctrl+Alt+Shift+Super to release the mouse");
    // run event loop
    event_server.run()?;

    Ok(())
}
