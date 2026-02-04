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
use lan_mouse_ipc::{IpcError, IpcListenerCreationError};
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
    #[cfg(windows)]
    {
        // Try to start as windows service first.
        // If it fails, it means we were not started by the SCM.
        if let Ok(_) = lan_mouse::windows_service::run_as_service() {
            return;
        }
    }

    // init logging
    let env = Env::default().filter_or("LAN_MOUSE_LOG_LEVEL", "info");
    
    #[cfg(windows)]
    {
        // If running as daemon without console (spawned by watchdog), log to file
        use windows::Win32::System::Console::GetConsoleWindow;
        
        let has_console = unsafe { !GetConsoleWindow().is_invalid() };
        
        if !has_console && std::env::args().any(|arg| arg == "daemon") {
            // No console - set up file logging for session daemon
            let log_dir = std::path::Path::new("C:\\ProgramData\\lan-mouse");
            let _ = std::fs::create_dir_all(log_dir);
            let log_path = log_dir.join("daemon.log");
            
            if let Ok(log_file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                env_logger::Builder::from_env(env)
                    .format_timestamp_secs()
                    .target(env_logger::Target::Pipe(Box::new(log_file)))
                    .init();
            } else {
                env_logger::init_from_env(env);
            }
        } else {
            env_logger::init_from_env(env);
        }
    }
    
    #[cfg(not(windows))]
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
            #[cfg(windows)]
            Command::Install => {
                lan_mouse::windows::install().map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            }
            #[cfg(windows)]
            Command::Uninstall => {
                lan_mouse::windows::uninstall().map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            }
            #[cfg(windows)]
            Command::Status => {
                lan_mouse::windows_service::service_status().map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            }
            #[cfg(windows)]
            Command::Watchdog => {
                // This should only be called by SCM, not directly by user
                log::error!("Watchdog mode should not be invoked directly - use 'lan-mouse install'");
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "Watchdog mode is internal").into());
            }
        },
        None => {
            //  otherwise start the service as a child process and
            //  run a frontend
            #[cfg(feature = "gtk")]
            {
                let mut service = start_service()?;
                let res = lan_mouse_gtk::run();
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

#[allow(dead_code)]
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
