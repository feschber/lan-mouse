use anyhow::Result;
use std::{thread, io::Write, net::SocketAddr};

#[cfg(unix)]
use std::{os::unix::net::UnixStream, path::Path, env};

use crate::client::Position;

use super::{FrontendEvent, Frontend};

pub struct CliFrontend;

impl Frontend for CliFrontend {}

impl CliFrontend {
    pub fn new() -> Result<CliFrontend> {
        #[cfg(unix)]
        let socket_path = Path::new(env::var("XDG_RUNTIME_DIR")?.as_str()).join("lan-mouse-socket.sock");
        thread::Builder::new()
            .name("cli-frontend".to_string())
            .spawn(move || {
            loop {
                eprint!("lan-mouse > ");
                std::io::stderr().flush().unwrap();
                let mut buf = String::new();
                match std::io::stdin().read_line(&mut buf) {
                    #[cfg(unix)]
                    Ok(len) => {
                        if let Some(event) = parse_event(buf, len) {
                            let Ok(mut stream) = UnixStream::connect(&socket_path) else {
                                log::error!("Could not connect to lan-mouse-socket");
                                continue;
                            };
                            let json = serde_json::to_string(&event).unwrap();
                            if let Err(e) = stream.write(json.as_bytes()) {
                                log::error!("error sending message: {e}");
                            };
                            if event == FrontendEvent::RequestShutdown() {
                                break;
                            }
                        }
                    }
                    #[cfg(windows)]
                    Ok(_) => {
                        log::warn!("not yet implemented!")
                    },
                    Err(e) => {
                        log::error!("{e:?}");
                        break
                    }
                }
            }
        }).unwrap();
        Ok(Self {})
    }
}

fn parse_event(s: String, len: usize) -> Option<FrontendEvent> {
    if len == 0 {
        return Some(FrontendEvent::RequestShutdown())
    }
    let mut l = s.split_whitespace();
    let cmd = l.next()?;
    match cmd {
        "connect" => {
            let addr = match l.next()?.parse() {
                Ok(addr) => SocketAddr::V4(addr),
                Err(e) => {
                    log::error!("parse error: {e}");
                    return None;
                }
            };
            Some(FrontendEvent::RequestClientAdd(addr, Position::Left ))
        }
        _ => {
            log::error!("unknown command: {s}");
            None
        }
    }
}
