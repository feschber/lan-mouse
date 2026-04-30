use env_logger::Env;
use input_capture::InputCaptureError;
use input_emulation::InputEmulationError;
use lan_mouse::{
    capture_test,
    config::{self, Command, Config, ConfigError},
    emulation_test,
    service::{Service, ServiceError},
};
use lan_mouse_cli::CliError;
#[cfg(feature = "gtk")]
use lan_mouse_gtk::GtkError;
use lan_mouse_ipc::{GuiLock, IpcError, IpcListenerCreationError};
use std::{
    future::Future,
    io,
    process::{self, Child},
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
    #[cfg(feature = "gtk")]
    #[error(transparent)]
    Gtk(#[from] GtkError),
    #[error(transparent)]
    Cli(#[from] CliError),
}

fn main() {
    // init logging
    let env = Env::default().filter_or("LAN_MOUSE_LOG_LEVEL", "info");
    env_logger::init_from_env(env);

    if let Err(e) = run() {
        log::error!("{e}");
        process::exit(1);
    }
}

fn run() -> Result<(), LanMouseError> {
    let config = config::Config::new()?;
    match config.command() {
        Some(command) => match command {
            Command::TestEmulation(args) => run_async(emulation_test::run(config, args))?,
            Command::TestCapture(args) => run_async(capture_test::run(config, args))?,
            Command::Cli(cli_args) => run_async(lan_mouse_cli::run(cli_args))?,
            Command::Daemon => {
                // if daemon is specified we run the service
                match run_async(run_service(config)) {
                    Err(LanMouseError::Service(ServiceError::IpcListen(
                        IpcListenerCreationError::AlreadyRunning,
                    ))) => log::info!("service already running!"),
                    r => r?,
                }
            }
        },
        None => {
            //  otherwise start the service as a child process and
            //  run a frontend
            #[cfg(feature = "gtk")]
            {
                // Cross-platform GUI singleton: only one Lan Mouse
                // window per user session. If another GUI is already
                // listening we send it a "show yourself" byte and
                // exit; otherwise we hold the lock for the GUI's
                // lifetime. Decoupled from the daemon socket so a
                // headless `lan-mouse daemon` doesn't block a later
                // GUI launch.
                let gui_lock = match GuiLock::acquire_or_signal() {
                    Ok(Some(lock)) => Some(lock),
                    Ok(None) => {
                        log::info!(
                            "lan-mouse GUI is already running; brought it to the foreground"
                        );
                        return Ok(());
                    }
                    Err(e) => {
                        // Don't fail the whole launch over the lock —
                        // log and proceed without singleton coverage.
                        log::warn!("could not acquire GUI singleton lock: {e}");
                        None
                    }
                };

                let mut service = start_service()?;
                let res = lan_mouse_gtk::run(gui_lock);

                // Bound the daemon-child cleanup so a wedged daemon
                // (CGEventTap stuck on macOS, hung syscall, etc.)
                // can't freeze the GUI on quit. SIGINT first, give it
                // a few seconds to exit cleanly, then SIGKILL.
                #[cfg(unix)]
                {
                    let pid = service.id() as libc::pid_t;
                    unsafe {
                        libc::kill(pid, libc::SIGINT);
                    }
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                    loop {
                        match service.try_wait() {
                            Ok(Some(_)) => break,
                            Ok(None) if std::time::Instant::now() >= deadline => {
                                log::warn!(
                                    "daemon child did not exit on SIGINT in 3s — sending SIGKILL"
                                );
                                let _ = service.kill();
                                let _ = service.wait();
                                break;
                            }
                            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                            Err(e) => {
                                log::error!("waiting for daemon child: {e}");
                                break;
                            }
                        }
                    }
                }
                #[cfg(not(unix))]
                {
                    let _ = service.kill();
                    let _ = service.wait();
                }
                res?;
            }
            #[cfg(not(feature = "gtk"))]
            {
                // run daemon if gtk is diabled
                match run_async(run_service(config)) {
                    Err(LanMouseError::Service(ServiceError::IpcListen(
                        IpcListenerCreationError::AlreadyRunning,
                    ))) => log::info!("service already running!"),
                    r => r?,
                }
            }
        }
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
    let child = process::Command::new(std::env::current_exe()?)
        .args(std::env::args().skip(1))
        .arg("daemon")
        .spawn()?;
    Ok(child)
}

async fn run_service(config: Config) -> Result<(), ServiceError> {
    let release_bind = config.release_bind();
    let config_path = config.config_path().to_owned();
    let mut service = Service::new(config).await?;
    log::info!("using config: {config_path:?}");
    log::info!("Press {release_bind:?} to release the mouse");
    service.run().await?;
    log::info!("service exited!");
    Ok(())
}
