use std::{process, error::Error};

use env_logger::Env;
use lan_mouse::{
    consumer, producer,
    config::{Config, Frontend::{Gtk, Cli}}, event::server::Server,
    frontend::{FrontendAdapter, cli, gtk},
};

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
    let frontend_adapter = FrontendAdapter::new()?;

    // start sending and receiving events
    let mut event_server = Server::new(config.port, producer, consumer, frontend_adapter)?;

    // add clients form config
    config.get_clients().into_iter().for_each(|(c, h, p)| {
        let host_name = match h {
            Some(h) => format!(" '{}'", h),
            None => "".to_owned(),
        };
        if c.len() == 0 {
            log::warn!("ignoring client{} with 0 assigned ips!", host_name);
        }
        log::info!("adding client [{}]{} @ {:?}", p, host_name, c);
        event_server.add_client(c, p);
    });

    // any threads need to be started after event_server sets up signal handling
    match config.frontend {
        #[cfg(all(unix, feature = "gtk"))]
        Gtk => { gtk::start()?; }
        #[cfg(not(feature = "gtk"))]
        Gtk => panic!("gtk frontend requested but feature not enabled!"),
        Cli => { cli::start()?; }
    };

    log::info!("Press Ctrl+Alt+Shift+Super to release the mouse");
    // run event loop
    event_server.run()?;

    Ok(())
}
