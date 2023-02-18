use std::{sync::mpsc, process, env};

use lan_mouse::{
    client::ClientManager,
    consumer, producer,
    config, event, request,
};

fn usage() {
    eprintln!("usage: {} [--backend <backend>] [--port <port>]",
        env::args().next().unwrap_or("lan-mouse".into()));
}

pub fn main() {
    // parse config file
    let config = match config::Config::new() {
        Err(e) => {
            eprintln!("{e}");
            usage();
            process::exit(1);
        }
        Ok(config) => config,
    };

    // port or default
    let port = config.port;

    // event channel for producing events
    let (produce_tx, produce_rx) = mpsc::sync_channel(128);

    // event channel for consuming events
    let (consume_tx, consume_rx) = mpsc::sync_channel(128);

    // create client manager
    let mut client_manager = match ClientManager::new(&config) {
        Err(e) => {
            eprintln!("{e}");
            process::exit(1);
        }
        Ok(m) => m,
    };

    // start receiving client connection requests
    let (request_server, request_thread) = request::Server::listen(port).unwrap();

    // start producing and consuming events
    let event_producer = producer::start(produce_tx, client_manager.get_clients(), request_server);
    let event_consumer = consumer::start(consume_rx, client_manager.get_clients(), config.backend);

    // start sending and receiving events
    let event_server = event::server::Server::new(port);
    let (receiver, sender) = event_server
        .run(&mut client_manager, produce_rx, consume_tx)
        .unwrap();

    request_thread.join().unwrap();

    receiver.join().unwrap();
    sender.join().unwrap();

    event_producer.join().unwrap();
    event_consumer.join().unwrap();
}
