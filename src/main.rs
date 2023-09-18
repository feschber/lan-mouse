use std::{process, error::Error};

use env_logger::Env;
use lan_mouse::{
    consumer, producer,
    config, event,
    frontend::{self, Frontend, FrontendAdapter},
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
    let config = config::Config::new()?;

    // start producing and consuming events
    let producer = producer::create()?;
    let consumer = consumer::create()?;

    // create frontend communication adapter
    let frontend_adapter = FrontendAdapter::new()?;

    log::info!("Press Ctrl+Alt+Shift+Super to release the mouse");

    // start sending and receiving events
    let mut event_server = event::server::Server::new(config.port, producer, consumer, frontend_adapter)?;

    // any threads need to be started after event_server sets up signal handling
    let _: Box<dyn Frontend> = match config.frontend {
        config::Frontend::Gtk => {
            #[cfg(all(unix, feature = "gtk"))]
            frontend::gtk::create();
            #[cfg(any(not(unix), not(feature = "gtk")))]
            panic!("gtk frontend requested but feature not enabled!");
        },
        config::Frontend::Cli => Box::new(frontend::cli::CliFrontend::new()?),
    };

    // run event loop
    event_server.run()?;

    Ok(())
}
