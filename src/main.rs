use anyhow::Result;
use std::process;

use env_logger::Env;
use lan_mouse::{
    consumer, producer,
    config::Config, server::Server,
    frontend::{FrontendListener, self},
};

use tokio::{task::LocalSet, join};

pub fn main() {

    // init logging
    let env = Env::default().filter_or("LAN_MOUSE_LOG_LEVEL", "info");
    env_logger::init_from_env(env);

    if let Err(e) = run() {
        log::error!("{e}");
        process::exit(1);
    }
}

pub fn run() -> Result<()> {
    // parse config file + cli args
    let config = Config::new()?;
    log::debug!("{config:?}");

    // start frontend
    if config.frontend_only {
        return frontend::run_frontend(&config);
    }

    // create single threaded tokio runtime
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;

    // run async event loop
    runtime.block_on(LocalSet::new().run_until(async {
        // create frontend communication adapter
        let frontend_adapter = match FrontendListener::new().await {
            Some(Err(e)) => return Err(e),
            Some(Ok(f)) => Some(f),
            None => None,
        };

        // start the frontend in a child process
        if !config.daemon_only {
            frontend::start_frontend()?;
        }

        let frontend_adapter = match frontend_adapter {
            Some(f) => f,
            // none means some other instance is already running
            None => return anyhow::Ok(()),
        };


        // create event producer and consumer
        let (producer, consumer) = join!(
            producer::create(),
            consumer::create(),
        );
        let (producer, consumer) = (producer?, consumer?);

        // create server
        let mut event_server = Server::new(config, frontend_adapter, consumer, producer).await?;
        log::info!("Press Ctrl+Alt+Shift+Super to release the mouse");

        // run event loop
        event_server.run().await?;
        anyhow::Ok(())
    }))?;
    log::debug!("exiting main");

    anyhow::Ok(())
}
