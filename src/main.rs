use std::{process, error::Error};

use env_logger::Env;
use lan_mouse::{
    client::ClientManager,
    consumer, producer,
    config, event, request,
    frontend::{self, Frontend},
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

    let mut frontend: Box<dyn Frontend> = match config.frontend {
        config::Frontend::Gtk => {
            #[cfg(all(unix, feature = "gtk"))]
            frontend::gtk::create();
            #[cfg(any(not(unix), not(feature = "gtk")))]
            panic!("gtk frontend requested but feature not enabled!");
        },
        config::Frontend::Cli => frontend::cli::create()?,
    };

    // create client manager
    let client_manager = ClientManager::new()?;

    // start receiving client connection requests
    let (_request_server, request_thread) = request::Server::listen(config.port)?;

    println!("Press Ctrl+Alt+Shift+Super to release the mouse");

    // start producing and consuming events
    let producer = producer::create()?;
    let consumer = consumer::create()?;

    // start sending and receiving events
    let event_server = event::server::Server::new(config.port)?;

    frontend.start();
    event_server.run(client_manager, producer, consumer, frontend)?;

    request_thread.join().unwrap();

    Ok(())
}
