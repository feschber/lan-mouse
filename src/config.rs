use anyhow::Result;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::net::IpAddr;
use std::{error::Error, fs};
use toml;

use crate::client::Position;

pub const DEFAULT_PORT: u16 = 4242;

#[derive(Serialize, Deserialize, Debug)]
pub struct ConfigToml {
    pub port: Option<u16>,
    pub frontend: Option<String>,
    pub left: Option<TomlClient>,
    pub right: Option<TomlClient>,
    pub top: Option<TomlClient>,
    pub bottom: Option<TomlClient>,
}

#[derive(Serialize, Deserialize, Debug, Eq, PartialEq)]
pub struct TomlClient {
    pub hostname: Option<String>,
    pub host_name: Option<String>,
    pub ips: Option<Vec<IpAddr>>,
    pub port: Option<u16>,
    pub activate_on_startup: Option<bool>,
}

impl ConfigToml {
    pub fn new(path: &str) -> Result<ConfigToml, Box<dyn Error>> {
        let config = fs::read_to_string(path)?;
        log::info!("using config: \"{path}\"");
        Ok(toml::from_str::<_>(&config)?)
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct CliArgs {
    /// the listen port for lan-mouse
    #[arg(short, long)]
    port: Option<u16>,

    /// the frontend to use [cli | gtk]
    #[arg(short, long)]
    frontend: Option<String>,

    /// non-default config file location
    #[arg(short, long)]
    config: Option<String>,

    /// run only the service as a daemon without the frontend
    #[arg(short, long)]
    daemon: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Frontend {
    Gtk,
    Cli,
}

#[derive(Debug)]
pub struct Config {
    pub frontend: Frontend,
    pub port: u16,
    pub clients: Vec<(TomlClient, Position)>,
    pub daemon: bool,
}

pub struct ConfigClient {
    pub ips: HashSet<IpAddr>,
    pub hostname: Option<String>,
    pub port: u16,
    pub pos: Position,
    pub active: bool,
}

impl Config {
    pub fn new() -> Result<Self> {
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

        let frontend = match args.frontend {
            None => match &config_toml {
                Some(c) => c.frontend.clone(),
                None => None,
            },
            frontend => frontend,
        };

        let frontend = match frontend {
            #[cfg(feature = "gtk")]
            None => Frontend::Gtk,
            #[cfg(not(feature = "gtk"))]
            None => Frontend::Cli,
            Some(s) => match s.as_str() {
                "cli" => Frontend::Cli,
                "gtk" => Frontend::Gtk,
                _ => Frontend::Cli,
            },
        };

        let port = match args.port {
            Some(port) => port,
            None => match &config_toml {
                Some(c) => c.port.unwrap_or(DEFAULT_PORT),
                None => DEFAULT_PORT,
            },
        };

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

        Ok(Config {
            daemon,
            frontend,
            clients,
            port,
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
                ConfigClient {
                    ips,
                    hostname,
                    port,
                    pos: *pos,
                    active,
                }
            })
            .collect()
    }
}
