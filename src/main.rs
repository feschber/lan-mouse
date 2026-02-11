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
    let config = match config::Config::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error loading config: {e}");
            process::exit(1);
        }
    };

    let command = config.command();
    init_logging(&command);

    if let Err(e) = run(config, command) {
        log::error!("{e}");
        process::exit(1);
    }
}

fn init_logging(_command: &Option<Command>) {
    let env = Env::default().filter_or("LAN_MOUSE_LOG_LEVEL", "info");

    #[cfg(windows)]
    {
        use windows::Win32::System::Console::GetConsoleWindow;
        let has_console = unsafe { !GetConsoleWindow().is_invalid() };

        let log_file_name = match _command {
            Some(Command::Daemon) if !has_console => Some("daemon.log"),
            Some(Command::WinSvc) => Some("winsvc.log"),
            _ => None,
        };

        if let Some(name) = log_file_name {
            let log_dir = std::path::Path::new("C:\\ProgramData\\lan-mouse");
            let _ = std::fs::create_dir_all(log_dir);
            let log_path = log_dir.join(name);

            if let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                env_logger::Builder::from_env(env)
                    .format_timestamp_secs()
                    .target(env_logger::Target::Pipe(Box::new(file)))
                    .init();
                return;
            }
        }
    }

    env_logger::init_from_env(env);
}

fn run(config: Config, command: Option<Command>) -> Result<(), LanMouseError> {
    match command {
        Some(command) => match command {
            Command::TestEmulation(args) => run_async(emulation_test::run(config, args))?,
            Command::TestCapture(args) => run_async(capture_test::run(config, args))?,
            Command::Cli(cli_args) => run_async(lan_mouse_cli::run(cli_args))?,
            Command::Daemon => {
                match run_async(run_daemon(config)) {
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
            Command::WinSvc => {
                // This starts the Windows service dispatcher
                lan_mouse::windows_service::run_dispatch().map_err(|e| {
                    io::Error::new(io::ErrorKind::Other, format!("Failed to start service dispatcher: {e}"))
                })?;
            }
        },
        None => {
            // Default behavior: GUI + Daemon
            run_gui_and_daemon(config)?;
        }
    }

    Ok(())
}

fn run_gui_and_daemon(_config: Config) -> Result<(), LanMouseError> {
    #[cfg(feature = "gtk")]
    {
        let mut daemon = start_daemon_process()?;
        let res = lan_mouse_gtk::run();

        #[cfg(unix)]
        {
            // give the daemon a chance to terminate gracefully
            let pid = daemon.id() as libc::pid_t;
            unsafe { libc::kill(pid, libc::SIGINT); }
            daemon.wait()?;
        }

        #[cfg(not(unix))]
        {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }

        res?;
    }

    #[cfg(not(feature = "gtk"))]
    {
        match run_async(run_daemon(_config)) {
            Err(LanMouseError::Service(ServiceError::IpcListen(
                IpcListenerCreationError::AlreadyRunning,
            ))) => log::info!("daemon already running!"),
            r => r?,
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

#[cfg(feature = "gtk")]
fn start_daemon_process() -> Result<Child, io::Error> {
    let child = process::Command::new(std::env::current_exe()?)
        .args(std::env::args().skip(1))
        .arg("daemon")
        .spawn()?;
    Ok(child)
}

async fn run_daemon(config: Config) -> Result<(), ServiceError> {
    let release_bind = config.release_bind();
    let config_path = config.config_path().to_owned();
    let mut service = Service::new(config).await?;
    log::info!("using config: {config_path:?}");
    log::info!("Press {release_bind:?} to release the mouse");
    service.run().await?;
    log::info!("daemon exited!");
    Ok(())
}
