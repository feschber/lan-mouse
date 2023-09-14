use std::{process, env, error::Error};

use lan_mouse::{
    client::ClientManager,
    consumer, producer,
    config, event, request,
};

pub fn main() {
    if let Err(e) = run() {
        log::error!("{e}");
        process::exit(1);
    }
}

pub fn run() -> Result<(), Box<dyn Error>> {
    // parse config file
    let config = config::Config::new()?;

    // port or default
    let port = config.port;

    // create client manager
    let client_manager = ClientManager::new()?;

    // start receiving client connection requests
    let (_request_server, request_thread) = request::Server::listen(port)?;

    println!("Press Ctrl+Alt+Shift+Super to release the mouse");

    // start producing and consuming events
    let producer = producer::create().unwrap();
    let consumer = consumer::create().unwrap();

    // start sending and receiving events
    let event_server = event::server::Server::new(port)?;

    event_server.run(client_manager, producer, consumer)?;

    request_thread.join().unwrap();

    Ok(())
}
