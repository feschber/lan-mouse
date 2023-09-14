use std::{process, env, error::Error};

use lan_mouse::{
    client::ClientManager,
    consumer, producer::{self, ThreadProducer},
    config, event, request,
};

fn usage() {
    eprintln!("usage: {} [--backend <backend>] [--port <port>]",
        env::args().next().unwrap_or("lan-mouse".into()));
}

pub fn main() {
}

pub fn run() -> Result<(), Box<dyn Error>> {
    // parse config file
    let config = config::Config::new()?;

    // port or default
    let port = config.port;

    // create client manager
    let client_manager = ClientManager::new()?;

    // start receiving client connection requests
    let (request_server, request_thread) = request::Server::listen(port)?;

    println!("Press Ctrl+Alt+Shift+Super to release the mouse");

    // start producing and consuming events
    let event_producer = producer::create().unwrap();
    let event_consumer = consumer::create().unwrap();

    // start sending and receiving events
    let event_server = event::server::Server::new(port)?;

    let (receiver, sender) = event_server.run(client_manager, consumer)?;

    request_thread.join().unwrap();

    // stop receiving events and terminate event-consumer
    receiver.join().unwrap()?;

    // stop producing events and terminate event-sender
    match event_producer {
        producer::EventProducer::Epoll(_) => {},
        producer::EventProducer::ThreadProducer(p) => p.stop(),
    }

    sender.join().unwrap()?;

}
