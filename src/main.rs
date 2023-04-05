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

    println!("Press Ctrl+Alt+Shift+Super to release the mouse");

    // start producing and consuming events
    let event_producer = match producer::start(produce_tx, client_manager.get_clients(), request_server) {
        Err(e) => {
            eprintln!("Could not start event producer: {e}");
            None
        },
        Ok(p) => Some(p),
    };
    let event_consumer = match consumer::start(consume_rx, client_manager.get_clients(), config.backend) {
        Err(e) => {
            eprintln!("Could not start event consumer: {e}");
            None
        },
        Ok(p) => Some(p),
    };

    if event_consumer.is_none() && event_producer.is_none() {
        process::exit(1);
    }

    // start sending and receiving events
    let event_server = match event::server::Server::new(port) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            process::exit(1);
        }
    };
    let (receiver, sender) = match event_server.run(&mut client_manager, produce_rx, consume_tx) {
        Ok((r,s)) => (r,s),
        Err(e) => {
            eprintln!("{e}");
            process::exit(1);
        }
    };

    request_thread.join().unwrap();

    // stop receiving events and terminate event-consumer
    receiver.join().unwrap();
    if let Some(thread) = event_consumer {
        thread.join().unwrap();
    }

    // stop producing events and terminate event-sender
    if let Some(thread) = event_producer {
        thread.join().unwrap();
    }
    sender.join().unwrap();
}
