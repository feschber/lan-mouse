use std::{process, error::Error};

use env_logger::Env;
use lan_mouse::{
    consumer, producer,
    config::{Config, Frontend::{Cli, Gtk}}, event::server::Server,
    frontend::{FrontendListener, cli},
};

#[cfg(all(unix, feature = "gtk"))]
use lan_mouse::frontend::gtk;
use tokio::task::LocalSet;

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

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;

    // run async event loop
    runtime.block_on(LocalSet::new().run_until(async {
        // create frontend communication adapter
        let frontend_adapter = FrontendListener::new().await?;

        // start frontend
        match config.frontend {
            #[cfg(all(unix, feature = "gtk"))]
            Gtk => { gtk::start()?; }
            #[cfg(any(not(feature = "gtk"), not(unix)))]
            Gtk => panic!("gtk frontend requested but feature not enabled!"),
            Cli => { cli::start()?; }
        };

        // start sending and receiving events
        let mut event_server = Server::new(config.port, frontend_adapter, consumer, producer).await?;

        // add clients from config
        for (c,h,port,p) in config.get_clients().into_iter() {
            event_server.add_client(h, c, port, p).await;
        }

        log::info!("Press Ctrl+Alt+Shift+Super to release the mouse");
        // run event loop
        event_server.run().await?;
        Result::<_, Box<dyn Error>>::Ok(())
    }))?;
    log::debug!("exiting main");

    Ok(())
}
