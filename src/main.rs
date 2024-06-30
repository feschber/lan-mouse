use anyhow::Result;
use std::process::{self, Child, Command};

use env_logger::Env;
use lan_mouse::{capture_test, config::Config, emulation_test, frontend, server::Server};

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
    log::info!("release bind: {:?}", config.release_bind);

    if config.test_capture {
        capture_test::run()?;
    } else if config.test_emulation {
        emulation_test::run()?;
    } else if config.daemon {
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
        log::info!("Press {:?} to release the mouse", config.release_bind);

        let server = Server::new(config);
        server.run(config.capture_backend).await?;

        log::debug!("service exiting");
        anyhow::Ok(())
    }))?;
    Ok(())
}
