use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env::{self, VarError};
use std::fmt::Display;
use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::{collections::HashSet, io};
use thiserror::Error;
use toml;

use lan_mouse_ipc::{Position, DEFAULT_PORT};

use input_event::scancode::{
    self,
    Linux::{KeyLeftAlt, KeyLeftCtrl, KeyLeftMeta, KeyLeftShift},
};

#[derive(Serialize, Deserialize, Debug)]
pub struct ConfigToml {
    pub capture_backend: Option<CaptureBackend>,
    pub emulation_backend: Option<EmulationBackend>,
    pub port: Option<u16>,
    pub frontend: Option<Frontend>,
    pub release_bind: Option<Vec<scancode::Linux>>,
    pub cert_path: Option<PathBuf>,
    pub left: Option<TomlClient>,
    pub right: Option<TomlClient>,
    pub top: Option<TomlClient>,
    pub bottom: Option<TomlClient>,
    pub authorized_fingerprints: Option<HashMap<String, String>>,
}

#[derive(Serialize, Deserialize, Debug, Eq, PartialEq)]
pub struct TomlClient {
    pub capture_backend: Option<CaptureBackend>,
    pub hostname: Option<String>,
    pub host_name: Option<String>,
    pub ips: Option<Vec<IpAddr>>,
    pub port: Option<u16>,
    pub activate_on_startup: Option<bool>,
    pub enter_hook: Option<String>,
}

impl ConfigToml {
    pub fn new(path: &Path) -> Result<ConfigToml, ConfigError> {
        let config = fs::read_to_string(path)?;
        log::info!("using config: \"{path:?}\"");
        Ok(toml::from_str::<_>(&config)?)
    }
}

#[derive(Parser, Debug)]
#[command(author, version=env!("GIT_DESCRIBE"), about, long_about = None)]
struct CliArgs {
    /// the listen port for lan-mouse
    #[arg(short, long)]
    port: Option<u16>,

    /// the frontend to use [cli | gtk]
    #[arg(short, long)]
    frontend: Option<Frontend>,

    /// non-default config file location
    #[arg(short, long)]
    config: Option<String>,

    /// run only the service as a daemon without the frontend
    #[arg(short, long)]
    daemon: bool,

    /// test input capture
    #[arg(long)]
    test_capture: bool,

    /// test input emulation
    #[arg(long)]
    test_emulation: bool,

    /// capture backend override
    #[arg(long)]
    capture_backend: Option<CaptureBackend>,

    /// emulation backend override
    #[arg(long)]
    emulation_backend: Option<EmulationBackend>,

    /// path to non-default certificate location
    #[arg(long)]
    cert_path: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
pub enum CaptureBackend {
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    InputCapturePortal,
    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    LayerShell,
    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    X11,
    #[cfg(windows)]
    Windows,
    #[cfg(target_os = "macos")]
    MacOs,
    Dummy,
}

impl Display for CaptureBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
            CaptureBackend::InputCapturePortal => write!(f, "input-capture-portal"),
            #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
            CaptureBackend::LayerShell => write!(f, "layer-shell"),
            #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
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
            #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
            CaptureBackend::InputCapturePortal => Self::InputCapturePortal,
            #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
            CaptureBackend::LayerShell => Self::LayerShell,
            #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
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
    #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
    Wlroots,
    #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
    Libei,
    #[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
    Xdp,
    #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
    X11,
    #[cfg(windows)]
    Windows,
    #[cfg(target_os = "macos")]
    MacOs,
    Dummy,
}

impl From<EmulationBackend> for input_emulation::Backend {
    fn from(backend: EmulationBackend) -> Self {
        match backend {
            #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
            EmulationBackend::Wlroots => Self::Wlroots,
            #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
            EmulationBackend::Libei => Self::Libei,
            #[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
            EmulationBackend::Xdp => Self::Xdp,
            #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
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
            #[cfg(all(unix, feature = "wayland", not(target_os = "macos")))]
            EmulationBackend::Wlroots => write!(f, "wlroots"),
            #[cfg(all(unix, feature = "libei", not(target_os = "macos")))]
            EmulationBackend::Libei => write!(f, "libei"),
            #[cfg(all(unix, feature = "xdg_desktop_portal", not(target_os = "macos")))]
            EmulationBackend::Xdp => write!(f, "xdg-desktop-portal"),
            #[cfg(all(unix, feature = "x11", not(target_os = "macos")))]
            EmulationBackend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            EmulationBackend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            EmulationBackend::MacOs => write!(f, "macos"),
            EmulationBackend::Dummy => write!(f, "dummy"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, ValueEnum)]
pub enum Frontend {
    Gtk,
    Cli,
}

impl Default for Frontend {
    fn default() -> Self {
        if cfg!(feature = "gtk") {
            Self::Gtk
        } else {
            Self::Cli
        }
    }
}

#[derive(Debug)]
pub struct Config {
    pub authorized_fingerprints: HashMap<String, String>,
    pub capture_backend: Option<CaptureBackend>,
    pub emulation_backend: Option<EmulationBackend>,
    pub frontend: Frontend,
    pub port: u16,
    pub clients: Vec<(TomlClient, Position)>,
    pub daemon: bool,
    pub release_bind: Vec<scancode::Linux>,
    pub test_capture: bool,
    pub test_emulation: bool,
    pub cert_path: PathBuf,
}

pub struct ConfigClient {
    pub ips: HashSet<IpAddr>,
    pub hostname: Option<String>,
    pub port: u16,
    pub pos: Position,
    pub active: bool,
    pub enter_hook: Option<String>,
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
        let args = CliArgs::parse();
        const CONFIG_FILE_NAME: &str = "config.toml";
        const CERT_FILE_NAME: &str = "lan-mouse.pem";

        #[cfg(unix)]
        let config_path = {
            let xdg_config_home =
                env::var("XDG_CONFIG_HOME").unwrap_or(format!("{}/.config", env::var("HOME")?));
            format!("{xdg_config_home}/lan-mouse/")
        };

        #[cfg(not(unix))]
        let config_path = {
            let app_data =
                env::var("LOCALAPPDATA").unwrap_or(format!("{}/.config", env::var("USERPROFILE")?));
            format!("{app_data}\\lan-mouse\\{config_file}")
        };

        let config_path = PathBuf::from(config_path);
        let config_file = config_path.join(CONFIG_FILE_NAME);

        // --config <file> overrules default location
        let config_file = args.config.map(PathBuf::from).unwrap_or(config_file);

        let mut config_toml = match ConfigToml::new(&config_file) {
            Err(e) => {
                log::warn!("{config_file:?}: {e}");
                log::warn!("Continuing without config file ...");
                None
            }
            Ok(c) => Some(c),
        };

        let frontend_arg = args.frontend;
        let frontend_cfg = config_toml.as_ref().and_then(|c| c.frontend);
        let frontend = frontend_arg.or(frontend_cfg).unwrap_or_default();

        let port = args
            .port
            .or(config_toml.as_ref().and_then(|c| c.port))
            .unwrap_or(DEFAULT_PORT);

        log::debug!("{config_toml:?}");
        let release_bind = config_toml
            .as_ref()
            .and_then(|c| c.release_bind.clone())
            .unwrap_or(Vec::from_iter(DEFAULT_RELEASE_KEYS.iter().cloned()));

        let capture_backend = args
            .capture_backend
            .or(config_toml.as_ref().and_then(|c| c.capture_backend));

        let emulation_backend = args
            .emulation_backend
            .or(config_toml.as_ref().and_then(|c| c.emulation_backend));

        let cert_path = args
            .cert_path
            .or(config_toml.as_ref().and_then(|c| c.cert_path.clone()))
            .unwrap_or(config_path.join(CERT_FILE_NAME));

        let authorized_fingerprints = config_toml
            .as_mut()
            .map(|c| std::mem::take(&mut c.authorized_fingerprints))
            .flatten()
            .unwrap_or_default();

        let mut clients: Vec<(TomlClient, Position)> = vec![];

        if let Some(config_toml) = config_toml {
            if let Some(c) = config_toml.right {
                clients.push((c, Position::Right))
            }
            if let Some(c) = config_toml.left {
                clients.push((c, Position::Left))
            }
            if let Some(c) = config_toml.top {
                clients.push((c, Position::Top))
            }
            if let Some(c) = config_toml.bottom {
                clients.push((c, Position::Bottom))
            }
        }

        let daemon = args.daemon;
        let test_capture = args.test_capture;
        let test_emulation = args.test_emulation;

        Ok(Config {
            authorized_fingerprints,
            capture_backend,
            emulation_backend,
            daemon,
            frontend,
            clients,
            port,
            release_bind,
            test_capture,
            test_emulation,
            cert_path,
        })
    }

    pub fn get_clients(&self) -> Vec<ConfigClient> {
        self.clients
            .iter()
            .map(|(c, pos)| {
                let port = c.port.unwrap_or(DEFAULT_PORT);
                let ips: HashSet<IpAddr> = if let Some(ips) = c.ips.as_ref() {
                    HashSet::from_iter(ips.iter().cloned())
                } else {
                    HashSet::new()
                };
                let hostname = match &c.hostname {
                    Some(h) => Some(h.clone()),
                    None => c.host_name.clone(),
                };
                let active = c.activate_on_startup.unwrap_or(false);
                let enter_hook = c.enter_hook.clone();
                ConfigClient {
                    ips,
                    hostname,
                    port,
                    pos: *pos,
                    active,
                    enter_hook,
                }
            })
            .collect()
    }
}
