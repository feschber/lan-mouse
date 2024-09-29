use env_logger::Env;
use input_capture::InputCaptureError;
use input_emulation::InputEmulationError;
use lan_mouse::{
    capture_test,
    config::{Config, ConfigError, Frontend},
    emulation_test,
    service::{Service, ServiceError},
};
use lan_mouse_ipc::IpcError;
use std::{
    future::Future,
    io,
    process::{self, Child, Command},
};
use thiserror::Error;
use tokio::task::LocalSet;

#[derive(Debug, Error)]
enum LanMouseError {
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    IpcError(#[from] IpcError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Capture(#[from] InputCaptureError),
    #[error(transparent)]
    Emulation(#[from] InputEmulationError),
}

pub fn main() {
    // init logging
    let env = Env::default().filter_or("LAN_MOUSE_LOG_LEVEL", "info");
    env_logger::init_from_env(env);

    if let Err(e) = run() {
        log::error!("{e}");
        process::exit(1);
    }
}

fn run() -> Result<(), LanMouseError> {
    // parse config file + cli args
    let config = Config::new()?;
    log::debug!("{config:?}");
    log::info!("release bind: {:?}", config.release_bind);

    if config.test_capture {
        run_async(capture_test::run(config))?;
    } else if config.test_emulation {
        run_async(emulation_test::run(config))?;
    } else if config.daemon {
        // if daemon is specified we run the service
        run_async(run_service(config))?;
    } else {
        //  otherwise start the service as a child process and
        //  run a frontend
        let mut service = start_service()?;
        run_frontend(&config)?;
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

    Ok(())
}

fn run_async<F, E>(f: F) -> Result<(), LanMouseError>
where
    F: Future<Output = Result<(), E>>,
    LanMouseError: From<E>,
{
    // create single threaded tokio runtime
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;

    // run async event loop
    Ok(runtime.block_on(LocalSet::new().run_until(f))?)
}

fn start_service() -> Result<Child, io::Error> {
    let child = Command::new(std::env::current_exe()?)
        .args(std::env::args().skip(1))
        .arg("--daemon")
        .spawn()?;
    Ok(child)
}

async fn run_service(config: Config) -> Result<(), ServiceError> {
    log::info!("Press {:?} to release the mouse", config.release_bind);
    let mut server = Service::new(config).await?;
    server.run().await?;
    log::info!("service exited!");
    Ok(())
}

fn run_frontend(config: &Config) -> Result<(), IpcError> {
    match config.frontend {
        #[cfg(feature = "gtk")]
        Frontend::Gtk => {
            lan_mouse_gtk::run();
        }
        #[cfg(not(feature = "gtk"))]
        Frontend::Gtk => panic!("gtk frontend requested but feature not enabled!"),
        Frontend::Cli => {
            lan_mouse_cli::run()?;
        }
    };
    Ok(())
}
