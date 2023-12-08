use anyhow::Result;
use std::process::{self, Command, Child};

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

pub fn start_service() -> Result<Child> {
    let child = Command::new(std::env::current_exe()?)
        .args(std::env::args().skip(1))
        .arg("--daemon")
        .spawn()?;
    Ok(child)
}


pub fn run() -> Result<()> {
    // parse config file + cli args
    let config = Config::new()?;
    log::debug!("{config:?}");

    if config.daemon {
        // if daemon is specified we run the service
        run_service(&config)?;
    } else {
        //  otherwise start the service as a child process and 
        //  run a frontend
        start_service()?;
        frontend::run_frontend(&config)?;
    }


    anyhow::Ok(())
}

fn run_service(config: &Config) -> Result<()> {
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
            Some(Ok(f)) => f,
            None => {
                // none means some other instance is already running
                log::debug!("service already running, exiting");
                return anyhow::Ok(())
            }
,
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
        log::debug!("service exiting");
        anyhow::Ok(())
    }))?;
    Ok(())
}
