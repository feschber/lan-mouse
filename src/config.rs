use serde::{Deserialize, Serialize};
use core::fmt;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::{error::Error, fs};

use std::env;
use toml;

use crate::client::Position;
use crate::dns;

pub const DEFAULT_PORT: u16 = 4242;

#[derive(Serialize, Deserialize, Debug)]
pub struct ConfigToml {
    pub port: Option<u16>,
    pub frontend: Option<String>,
    pub left: Option<Client>,
    pub right: Option<Client>,
    pub top: Option<Client>,
    pub bottom: Option<Client>,
}

#[derive(Serialize, Deserialize, Debug, Eq, PartialEq)]
pub struct Client {
    pub host_name: Option<String>,
    pub ips: Option<Vec<IpAddr>>,
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

pub enum Frontend {
    Gtk,
    Cli,
}

pub struct Config {
    pub frontend: Frontend,
    pub port: u16,
    pub clients: Vec<(Client, Position)>,
}

impl Config {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let config_path = "config.toml";
        let config_toml = match ConfigToml::new(config_path) {
            Err(e) => {
                log::error!("config.toml: {e}");
                log::warn!("Continuing without config file ...");
                None
            },
            Ok(c) => Some(c),
        };

        let frontend = match find_arg("--frontend")? {
            None => match &config_toml {
                Some(c) => c.frontend.clone(),
                None => None,
            },
            frontend => frontend,
        };

        let frontend = match frontend {
            #[cfg(all(unix, feature = "gtk"))]
            None => Frontend::Gtk,
            #[cfg(any(not(feature = "gtk"), not(unix)))]
            None => Frontend::Cli,
            Some(s) => match s.as_str() {
                "cli" => Frontend::Cli,
                "gtk" => Frontend::Gtk,
                    _ => Frontend::Cli,
            }
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

        Ok(Config { frontend, clients, port })
    }

    pub fn get_clients(&self) -> Vec<(HashSet<SocketAddr>, Option<String>,  Position)> {
        self.clients.iter().map(|(c,p)| {
            let port = c.port.unwrap_or(DEFAULT_PORT);
            // add ips from config
            let config_ips: Vec<IpAddr> = if let Some(ips) = c.ips.as_ref() {
                ips.iter().cloned().collect()
            } else {
                vec![]
            };
            let host_name = c.host_name.clone();
            // add ips from dns lookup
            let dns_ips = match host_name.as_ref() {
                None => vec![],
                Some(host_name) => match dns::resolve(host_name) {
                    Err(e) => {
                        log::warn!("{host_name}: could not resolve host: {e}");
                        vec![]
                    }
                    Ok(l) if l.is_empty() => {
                        log::warn!("{host_name}: could not resolve host");
                        vec![]
                    }
                    Ok(l) => l,
                }
            };
            if config_ips.is_empty() && dns_ips.is_empty() {
                log::error!("no ips found for client {p:?}, ignoring!");
                log::error!("You can manually specify ip addresses via the `ips` config option");
            }
            let ips = config_ips.into_iter().chain(dns_ips.into_iter());

            // map ip addresses to socket addresses
            let addrs: HashSet<SocketAddr> = ips.map(|ip| SocketAddr::new(ip, port)).collect();
            (addrs, host_name, *p)
        }).filter(|(a, _, _)| !a.is_empty()).collect()
    }
}
