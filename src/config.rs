use serde_derive::{Deserialize, Serialize};
use core::fmt;
use std::net::IpAddr;
use std::{error::Error, fs};

use std::env;
use toml;

use crate::client::Position;

pub const DEFAULT_PORT: u16 = 4242;

#[derive(Serialize, Deserialize, Debug)]
pub struct ConfigToml {
    pub port: Option<u16>,
    pub backend: Option<String>,
    pub left: Option<Client>,
    pub right: Option<Client>,
    pub top: Option<Client>,
    pub bottom: Option<Client>,
}

#[derive(Serialize, Deserialize, Debug, Eq, PartialEq)]
pub struct Client {
    pub host_name: Option<String>,
    pub ip: Option<IpAddr>,
    pub port: Option<u16>,
}

#[derive(Debug, Clone)]
struct MissingParameter {
    arg: &'static str,
}

impl fmt::Display for MissingParameter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Missing a parameter for argument: {}", self.arg)
    }
}

impl Error for MissingParameter {}

impl ConfigToml {
    pub fn new(path: &str) -> Result<ConfigToml, Box<dyn Error>> {
        let config = fs::read_to_string(path)?;
        Ok(toml::from_str::<_>(&config)?)
    }
}

fn find_arg(key: &'static str) -> Result<Option<String>, MissingParameter> {
    let args: Vec<String> = env::args().collect();

    for (i, arg) in args.iter().enumerate() {
        if arg != key {
            continue;
        }
        match args.get(i+1) {
            None => return Err(MissingParameter { arg: key }),
            Some(arg) => return Ok(Some(arg.clone())),
        };
    }
    Ok(None)
}

pub struct Config {
    pub backend: Option<String>,
    pub port: u16,
    pub clients: Vec<(Client, Position)>,
}

impl Config {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let config_path = "config.toml";
        let config_toml = match ConfigToml::new(config_path) {
            Err(e) => {
                log::warn!("config.toml: {e}");
                log::warn!("Continuing without config file ...");
                None
            },
            Ok(c) => Some(c),
        };

        let backend = match find_arg("--backend")? {
            None => match &config_toml {
                Some(c) => c.backend.clone(),
                None => None,
            },
            backend => backend,
        };

        let port = match find_arg("--port")? {
            Some(port) => port.parse::<u16>()?,
            None => match &config_toml {
                Some(c) => c.port.unwrap_or(DEFAULT_PORT),
                None => DEFAULT_PORT,
            }
        };

        let mut clients: Vec<(Client, Position)> = vec![];

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

        Ok(Config { backend, clients, port })
    }
}
