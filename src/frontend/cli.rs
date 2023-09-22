use anyhow::{anyhow, Result};
use std::{thread::{self, JoinHandle}, io::{Write, Read}};
#[cfg(windows)]
use std::net::SocketAddrV4;

#[cfg(unix)]
use std::{os::unix::net::UnixStream, path::Path, env};
#[cfg(windows)]
use std::net::TcpStream;

use crate::{client::Position, config::DEFAULT_PORT};

use super::{FrontendEvent, FrontendNotify};

pub fn start() -> Result<(JoinHandle<()>, JoinHandle<()>)> {
    #[cfg(unix)]
    let socket_path = Path::new(env::var("XDG_RUNTIME_DIR")?.as_str()).join("lan-mouse-socket.sock");

    #[cfg(unix)]
    let Ok(mut tx) = UnixStream::connect(&socket_path) else {
        return Err(anyhow!("Could not connect to lan-mouse-socket"));
    };

    #[cfg(windows)]
    let Ok(mut stream) = TcpStream::connect("127.0.0.1:5252".parse::<SocketAddrV4>().unwrap()) else {
        log::error!("Could not connect to lan-mouse-server");
        continue;
    };

    let mut rx = tx.try_clone()?;

    let reader = thread::Builder::new()
        .name("cli-frontend".to_string())
        .spawn(move || {
        loop {
            eprint!("lan-mouse > ");
            std::io::stderr().flush().unwrap();
            let mut buf = String::new();
            match std::io::stdin().read_line(&mut buf) {
                Ok(len) => {
                    if let Some(event) = parse_cmd(buf, len) {
                        let json = serde_json::to_string(&event).unwrap();
                        let bytes = json.as_bytes();
                        let len = bytes.len().to_ne_bytes();
                        if let Err(e) = tx.write(&len) {
                            log::error!("error sending message: {e}");
                        };
                        if let Err(e) = tx.write(bytes) {
                            log::error!("error sending message: {e}");
                        };
                        if event == FrontendEvent::Shutdown() {
                            break;
                        }
                    }
                }
                Err(e) => {
                    log::error!("{e:?}");
                    break
                }
            }
        }
    })?;

    let writer = thread::Builder::new()
        .name("cli-frontend-notify".to_string())
        .spawn(move || {
            loop {
                // read len
                let mut len = [0u8; 8];
                match rx.read_exact(&mut len) {
                    Ok(()) => (),
                    Err(e) => break log::error!("{e}"),
                };
                let len = usize::from_ne_bytes(len);

                // read payload
                let mut buf: Vec<u8> = vec![0u8; len];
                match rx.read_exact(&mut buf[..len]) {
                    Ok(()) => (),
                    Err(e) => break log::error!("{e}"),
                };

                let notify: FrontendNotify = match serde_json::from_slice(&buf) {
                    Ok(n) => n,
                    Err(e) => break log::error!("{e}"),
                };
                match notify {
                    FrontendNotify::NotifyClientCreate(client, host, port, pos) => {
                        log::info!("new client ({client}): {}:{port} - {pos}", host.as_deref().unwrap_or(""));
                    },
                    FrontendNotify::NotifyClientUpdate(client, host, port, pos) => {
                        log::info!("client ({client}) updated: {}:{port} - {pos}", host.as_deref().unwrap_or(""));
                    },
                    FrontendNotify::NotifyClientDelete(client) => {
                        log::info!("client ({client}) deleted.");
                    },
                    FrontendNotify::NotifyError(e) => {
                        log::warn!("{e}");
                    },
                    FrontendNotify::Enumerate(e) => {
                        log::info!("{e:#?}");
                    }
                }
            }
        })?;
    Ok((reader, writer))
}

fn parse_cmd(s: String, len: usize) -> Option<FrontendEvent> {
    if len == 0 {
        return Some(FrontendEvent::Shutdown())
    }
    let mut l = s.split_whitespace();
    let cmd = l.next()?;
    match cmd {
        "help" => {
            println!("list                                                 list clients");
            println!("connect <host> left|right|top|bottom [port]          add a new client");
            println!("disconnect <client>                                  remove a client");
            println!("activate <client>                                    activate a client");
            println!("deactivate <client>                                  deactivate a client");
            println!("exit                                                 exit lan-mouse");
            None
        }
        "exit" => Some(FrontendEvent::Shutdown()),
        "list" => Some(FrontendEvent::Enumerate()),
        "connect" => {
            let host = l.next()?.to_owned();
            let pos = match l.next()? {
                "right" => Position::Right,
                "top" => Position::Top,
                "bottom" => Position::Bottom,
                _ => Position::Left,
            };
            let port = match l.next() {
                Some(p) => match p.parse() {
                    Ok(p) => p,
                    Err(e) => {
                        log::error!("{e}");
                        return None;
                    }
                }
                None => DEFAULT_PORT,
            };
            Some(FrontendEvent::AddClient(Some(host), port, pos))
        }
        "disconnect" => {
            let client = match l.next()?.parse() {
                Ok(p) => p,
                Err(e) => {
                    log::error!("{e}");
                    return None;
                }
            };
            Some(FrontendEvent::DelClient(client))
        }
        "activate" => {
            let client = match l.next()?.parse() {
                Ok(c) => c,
                Err(e) => {
                    log::error!("{e}");
                    return None;
                }
            };
            Some(FrontendEvent::ActivateClient(client, true))
        }
        "deactivate" => {
            let client = match l.next()?.parse() {
                Ok(c) => c,
                Err(e) => {
                    log::error!("{e}");
                    return None;
                }
            };
            Some(FrontendEvent::ActivateClient(client, false))
        }
        _ => {
            log::error!("unknown command: {s}");
            None
        }
    }
}
