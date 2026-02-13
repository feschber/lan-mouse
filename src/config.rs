use crate::capture_test::TestCaptureArgs;
use crate::emulation_test::TestEmulationArgs;
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env::{self, VarError};
use std::fmt::Display;
use std::fs::{self, File};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::{collections::HashSet, io};
use thiserror::Error;
use toml;
use toml_edit::{self, DocumentMut};

use lan_mouse_cli::CliArgs;
use lan_mouse_ipc::{DEFAULT_PORT, Position};

use input_event::scancode::{
    self,
    Linux::{KeyLeftAlt, KeyLeftCtrl, KeyLeftMeta, KeyLeftShift},
};

use shadow_rs::shadow;

shadow!(build);

const CONFIG_FILE_NAME: &str = "config.toml";
const CERT_FILE_NAME: &str = "lan-mouse.pem";

fn default_path() -> Result<PathBuf, VarError> {
    #[cfg(unix)]
    let default_path = {
        let xdg_config_home =
            env::var("XDG_CONFIG_HOME").unwrap_or(format!("{}/.config", env::var("HOME")?));
        format!("{xdg_config_home}/lan-mouse/")
    };

    #[cfg(not(unix))]
    let default_path = {
        #[cfg(windows)]
        if crate::is_windows_service() {
            "C:\\ProgramData\\lan-mouse\\".to_string()
        } else {
            let app_data =
                env::var("LOCALAPPDATA").unwrap_or(format!("{}/.config", env::var("USERPROFILE")?));
            format!("{app_data}\\lan-mouse\\")
        }
        #[cfg(not(windows))]
        {
            let app_data =
                env::var("LOCALAPPDATA").unwrap_or(format!("{}/.config", env::var("USERPROFILE")?));
            format!("{app_data}\\lan-mouse\\")
        }
    };
    Ok(PathBuf::from(default_path))
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct ConfigToml {
    capture_backend: Option<CaptureBackend>,
    emulation_backend: Option<EmulationBackend>,
    port: Option<u16>,
    release_bind: Option<Vec<scancode::Linux>>,
    cert_path: Option<PathBuf>,
    clients: Option<Vec<TomlClient>>,
    authorized_fingerprints: Option<HashMap<String, String>>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Eq, PartialEq)]
struct TomlClient {
    hostname: Option<String>,
    host_name: Option<String>,
    ips: Option<Vec<IpAddr>>,
    port: Option<u16>,
    position: Option<Position>,
    activate_on_startup: Option<bool>,
    enter_hook: Option<String>,
}

impl ConfigToml {
    fn new(path: &Path) -> Result<ConfigToml, ConfigError> {
        let config = fs::read_to_string(path)?;
        Ok(toml::from_str::<_>(&config)?)
    }
}

#[derive(Parser, Debug)]
#[command(author, version=build::CLAP_LONG_VERSION, about, long_about = None)]
struct Args {
    /// the listen port for lan-mouse
    #[arg(short, long)]
    port: Option<u16>,

    /// non-default config file location
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// capture backend override
    #[arg(long)]
    capture_backend: Option<CaptureBackend>,

    /// emulation backend override
    #[arg(long)]
    emulation_backend: Option<EmulationBackend>,

    /// path to non-default certificate location
    #[arg(long)]
    cert_path: Option<PathBuf>,

    /// subcommands
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// test input emulation
    TestEmulation(TestEmulationArgs),
    /// test input capture
    TestCapture(TestCaptureArgs),
    /// Lan Mouse commandline interface
    Cli(CliArgs),
    /// run in daemon mode
    Daemon,
    /// Install as system service (Windows: SCM service, Linux: systemd, macOS: launchd)
    #[cfg(windows)]
    Install,
    /// Uninstall system service
    #[cfg(windows)]
    Uninstall,
    /// Query service status
    #[cfg(windows)]
    Status,
    /// Run as Windows service (internal - spawns session daemons)
    #[cfg(windows)]
    #[command(hide = true)]
    WinSvc,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
pub enum CaptureBackend {
    #[cfg(all(unix, feature = "libei_capture", not(target_os = "macos")))]
    #[serde(rename = "input-capture-portal")]
    InputCapturePortal,
    #[cfg(all(unix, feature = "layer_shell_capture", not(target_os = "macos")))]
    #[serde(rename = "layer-shell")]
    LayerShell,
    #[cfg(all(unix, feature = "x11_capture", not(target_os = "macos")))]
    #[serde(rename = "x11")]
    X11,
    #[cfg(windows)]
    #[serde(rename = "windows")]
    Windows,
    #[cfg(target_os = "macos")]
    #[serde(rename = "macos")]
    MacOs,
    #[serde(rename = "dummy")]
    Dummy,
}

impl Display for CaptureBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(all(unix, feature = "libei_capture", not(target_os = "macos")))]
            CaptureBackend::InputCapturePortal => write!(f, "input-capture-portal"),
            #[cfg(all(unix, feature = "layer_shell_capture", not(target_os = "macos")))]
            CaptureBackend::LayerShell => write!(f, "layer-shell"),
            #[cfg(all(unix, feature = "x11_capture", not(target_os = "macos")))]
            CaptureBackend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            CaptureBackend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            CaptureBackend::MacOs => write!(f, "MacOS"),
            CaptureBackend::Dummy => write!(f, "dummy"),
        }
    }
}

impl From<CaptureBackend> for input_capture::Backend {
    fn from(backend: CaptureBackend) -> Self {
        match backend {
            #[cfg(all(unix, feature = "libei_capture", not(target_os = "macos")))]
            CaptureBackend::InputCapturePortal => Self::InputCapturePortal,
            #[cfg(all(unix, feature = "layer_shell_capture", not(target_os = "macos")))]
            CaptureBackend::LayerShell => Self::LayerShell,
            #[cfg(all(unix, feature = "x11_capture", not(target_os = "macos")))]
            CaptureBackend::X11 => Self::X11,
            #[cfg(windows)]
            CaptureBackend::Windows => Self::Windows,
            #[cfg(target_os = "macos")]
            CaptureBackend::MacOs => Self::MacOs,
            CaptureBackend::Dummy => Self::Dummy,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
pub enum EmulationBackend {
    #[cfg(all(unix, feature = "wlroots_emulation", not(target_os = "macos")))]
    #[serde(rename = "wlroots")]
    Wlroots,
    #[cfg(all(unix, feature = "libei_emulation", not(target_os = "macos")))]
    #[serde(rename = "libei")]
    Libei,
    #[cfg(all(unix, feature = "rdp_emulation", not(target_os = "macos")))]
    #[serde(rename = "xdp")]
    Xdp,
    #[cfg(all(unix, feature = "x11_emulation", not(target_os = "macos")))]
    #[serde(rename = "x11")]
    X11,
    #[cfg(windows)]
    #[serde(rename = "windows")]
    Windows,
    #[cfg(target_os = "macos")]
    #[serde(rename = "macos")]
    MacOs,
    #[serde(rename = "dummy")]
    Dummy,
}

impl From<EmulationBackend> for input_emulation::Backend {
    fn from(backend: EmulationBackend) -> Self {
        match backend {
            #[cfg(all(unix, feature = "wlroots_emulation", not(target_os = "macos")))]
            EmulationBackend::Wlroots => Self::Wlroots,
            #[cfg(all(unix, feature = "libei_emulation", not(target_os = "macos")))]
            EmulationBackend::Libei => Self::Libei,
            #[cfg(all(unix, feature = "rdp_emulation", not(target_os = "macos")))]
            EmulationBackend::Xdp => Self::Xdp,
            #[cfg(all(unix, feature = "x11_emulation", not(target_os = "macos")))]
            EmulationBackend::X11 => Self::X11,
            #[cfg(windows)]
            EmulationBackend::Windows => Self::Windows,
            #[cfg(target_os = "macos")]
            EmulationBackend::MacOs => Self::MacOs,
            EmulationBackend::Dummy => Self::Dummy,
        }
    }
}

impl Display for EmulationBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(all(unix, feature = "wlroots_emulation", not(target_os = "macos")))]
            EmulationBackend::Wlroots => write!(f, "wlroots"),
            #[cfg(all(unix, feature = "libei_emulation", not(target_os = "macos")))]
            EmulationBackend::Libei => write!(f, "libei"),
            #[cfg(all(unix, feature = "rdp_emulation", not(target_os = "macos")))]
            EmulationBackend::Xdp => write!(f, "xdg-desktop-portal"),
            #[cfg(all(unix, feature = "x11_emulation", not(target_os = "macos")))]
            EmulationBackend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            EmulationBackend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            EmulationBackend::MacOs => write!(f, "macos"),
            EmulationBackend::Dummy => write!(f, "dummy"),
        }
    }
}

#[derive(Debug)]
pub struct Config {
    /// command line arguments
    args: Args,
    /// path to the certificate file used
    cert_path: PathBuf,
    /// path to the config file used
    config_path: PathBuf,
    /// the (optional) toml config and it's path
    config_toml: Option<ConfigToml>,
}

pub struct ConfigClient {
    pub ips: HashSet<IpAddr>,
    pub hostname: Option<String>,
    pub port: u16,
    pub pos: Position,
    pub active: bool,
    pub enter_hook: Option<String>,
}

impl From<TomlClient> for ConfigClient {
    fn from(toml: TomlClient) -> Self {
        let active = toml.activate_on_startup.unwrap_or(false);
        let enter_hook = toml.enter_hook;
        let hostname = toml.hostname;
        let ips = HashSet::from_iter(toml.ips.into_iter().flatten());
        let port = toml.port.unwrap_or(DEFAULT_PORT);
        let pos = toml.position.unwrap_or_default();
        Self {
            ips,
            hostname,
            port,
            pos,
            active,
            enter_hook,
        }
    }
}

impl From<ConfigClient> for TomlClient {
    fn from(client: ConfigClient) -> Self {
        let hostname = client.hostname;
        let host_name = None;
        let mut ips = client.ips.into_iter().collect::<Vec<_>>();
        ips.sort();
        let ips = Some(ips);
        let port = if client.port == DEFAULT_PORT {
            None
        } else {
            Some(client.port)
        };
        let position = Some(client.pos);
        let activate_on_startup = if client.active { Some(true) } else { None };
        let enter_hook = client.enter_hook;
        Self {
            hostname,
            host_name,
            ips,
            port,
            position,
            activate_on_startup,
            enter_hook,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Var(#[from] VarError),
}

const DEFAULT_RELEASE_KEYS: [scancode::Linux; 4] =
    [KeyLeftCtrl, KeyLeftShift, KeyLeftMeta, KeyLeftAlt];

impl Config {
    pub fn new() -> Result<Self, ConfigError> {
        let args = Args::parse();
        Self::from_args(args)
    }

    pub fn new_with_args<I, T>(args_iter: I) -> Result<Self, ConfigError>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let args = Args::parse_from(args_iter);
        Self::from_args(args)
    }

    fn from_args(args: Args) -> Result<Self, ConfigError> {
        #[cfg(windows)]
        if matches!(args.command, Some(Command::WinSvc)) {
            crate::set_is_windows_service(true);
        }

        // --config <file> overrules default location
        let config_path = args
            .config
            .clone()
            .unwrap_or(default_path()?.join(CONFIG_FILE_NAME));

        let config_toml = match ConfigToml::new(&config_path) {
            Err(e) => {
                log::warn!("{config_path:?}: {e}");
                log::warn!("Continuing without config file ...");
                None
            }
            Ok(c) => Some(c),
        };

        // --cert-path <file> overrules default location
        let cert_path = args
            .cert_path
            .clone()
            .or(config_toml.as_ref().and_then(|c| c.cert_path.clone()))
            .unwrap_or(default_path()?.join(CERT_FILE_NAME));

        Ok(Config {
            args,
            cert_path,
            config_path,
            config_toml,
        })
    }

    /// the command to run
    pub fn command(&self) -> Option<Command> {
        self.args.command.clone()
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// public key fingerprints authorized for connection
    pub fn authorized_fingerprints(&self) -> HashMap<String, String> {
        self.config_toml
            .as_ref()
            .and_then(|c| c.authorized_fingerprints.clone())
            .unwrap_or_default()
    }

    /// path to certificate
    pub fn cert_path(&self) -> &Path {
        &self.cert_path
    }

    /// optional input-capture backend override
    pub fn capture_backend(&self) -> Option<CaptureBackend> {
        self.args
            .capture_backend
            .or(self.config_toml.as_ref().and_then(|c| c.capture_backend))
    }

    /// optional input-emulation backend override
    pub fn emulation_backend(&self) -> Option<EmulationBackend> {
        self.args
            .emulation_backend
            .or(self.config_toml.as_ref().and_then(|c| c.emulation_backend))
    }

    /// the port to use (initially)
    pub fn port(&self) -> u16 {
        self.args
            .port
            .or(self.config_toml.as_ref().and_then(|c| c.port))
            .unwrap_or(DEFAULT_PORT)
    }

    /// list of configured clients
    pub fn clients(&self) -> Vec<ConfigClient> {
        self.config_toml
            .as_ref()
            .map(|c| c.clients.clone())
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .map(From::<TomlClient>::from)
            .collect()
    }

    /// release bind for returning control to the host
    pub fn release_bind(&self) -> Vec<scancode::Linux> {
        self.config_toml
            .as_ref()
            .and_then(|c| c.release_bind.clone())
            .unwrap_or(Vec::from_iter(DEFAULT_RELEASE_KEYS.iter().cloned()))
    }

    /// set configured clients
    pub fn set_clients(&mut self, clients: Vec<ConfigClient>) {
        if clients.is_empty() {
            return;
        }
        if self.config_toml.is_none() {
            self.config_toml = Some(Default::default());
        }
        self.config_toml.as_mut().expect("config").clients =
            Some(clients.into_iter().map(|c| c.into()).collect::<Vec<_>>());
    }

    /// set authorized keys
    pub fn set_authorized_keys(&mut self, fingerprints: HashMap<String, String>) {
        if fingerprints.is_empty() {
            return;
        }
        if self.config_toml.is_none() {
            self.config_toml = Default::default();
        }
        self.config_toml
            .as_mut()
            .expect("config")
            .authorized_fingerprints = Some(fingerprints);
    }

    pub fn write_back(&self) -> Result<(), io::Error> {
        log::info!("writing config to {:?}", &self.config_path);
        /* load the current configuration file */
        let current_config = match fs::read_to_string(&self.config_path) {
            Ok(c) => c.parse::<DocumentMut>().unwrap_or_default(),
            Err(e) => {
                log::info!("{:?} {e} => creating new config", self.config_path());
                Default::default()
            }
        };
        let _current_config =
            toml_edit::de::from_document::<ConfigToml>(current_config).unwrap_or_default();

        /* the new config */
        let new_config = self.config_toml.clone().unwrap_or_default();
        // let new_config = toml_edit::ser::to_document::<ConfigToml>(&new_config).expect("fixme");
        let new_config = toml_edit::ser::to_string_pretty(&new_config).expect("config");

        /*
         * TODO merge documents => eventually we might want to split this up into clients configured
         * via the config file and clients managed through the GUI / frontend.
         * The latter should be saved to $XDG_DATA_HOME instead of $XDG_CONFIG_HOME,
         * and clients configured through .config could be made permanent.
         * For now we just override the config file.
         */

        /* write new config to file */
        if let Some(p) = self.config_path().parent() {
            fs::create_dir_all(p)?;
        }
        let mut f = File::create(self.config_path())?;
        f.write_all(new_config.as_bytes())?;

        Ok(())
    }
}
