use anyhow::Result;
use std::process::{self, Child, Command};

use env_logger::Env;
use lan_mouse::{config::Config, frontend, server::Server};

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
        let mut service = start_service()?;
        frontend::run_frontend(&config)?;
        #[cfg(unix)]
        {
            // on unix we give the service a chance to terminate gracefully
            let pid = service.id() as libc::pid_t;
            unsafe {
                libc::kill(pid, libc::SIGINT);
            }
            service.wait()?;
        }
        service.kill()?;
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
        // run main loop
        log::info!("Press Ctrl+Alt+Shift+Super to release the mouse");

        let server = Server::new(config);
        server.run().await?;

        log::debug!("service exiting");
        anyhow::Ok(())
    }))?;
    Ok(())
}
