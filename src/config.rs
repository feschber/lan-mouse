use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use std::env::{self, VarError};
use std::fmt::Display;
use std::fs;
use std::net::IpAddr;
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
    pub left: Option<TomlClient>,
    pub right: Option<TomlClient>,
    pub top: Option<TomlClient>,
    pub bottom: Option<TomlClient>,
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
    pub fn new(path: &str) -> Result<ConfigToml, ConfigError> {
        let config = fs::read_to_string(path)?;
        log::info!("using config: \"{path}\"");
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

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize, ValueEnum)]
pub enum Frontend {
    #[serde(rename = "gtk")]
    Gtk,
    #[serde(rename = "cli")]
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
    pub capture_backend: Option<CaptureBackend>,
    pub emulation_backend: Option<EmulationBackend>,
    pub frontend: Frontend,
    pub port: u16,
    pub clients: Vec<(TomlClient, Position)>,
    pub daemon: bool,
    pub release_bind: Vec<scancode::Linux>,
    pub test_capture: bool,
    pub test_emulation: bool,
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
        let config_file = "config.toml";
        #[cfg(unix)]
        let config_path = {
            let xdg_config_home =
                env::var("XDG_CONFIG_HOME").unwrap_or(format!("{}/.config", env::var("HOME")?));
            format!("{xdg_config_home}/lan-mouse/{config_file}")
        };

        #[cfg(not(unix))]
        let config_path = {
            let app_data =
                env::var("LOCALAPPDATA").unwrap_or(format!("{}/.config", env::var("USERPROFILE")?));
            format!("{app_data}\\lan-mouse\\{config_file}")
        };

        // --config <file> overrules default location
        let config_path = args.config.unwrap_or(config_path);

        let config_toml = match ConfigToml::new(config_path.as_str()) {
            Err(e) => {
                log::warn!("{config_path}: {e}");
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
            capture_backend,
            emulation_backend,
            daemon,
            frontend,
            clients,
            port,
            release_bind,
            test_capture,
            test_emulation,
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
