use anyhow::Result;
use std::{thread::{self, JoinHandle}, io::Write};
#[cfg(windows)]
use std::net::SocketAddrV4;

#[cfg(unix)]
use std::{os::unix::net::UnixStream, path::Path, env};
#[cfg(windows)]
use std::net::TcpStream;

use crate::{client::Position, config::DEFAULT_PORT};

use super::FrontendEvent;

pub fn start() -> Result<JoinHandle<()>> {
    #[cfg(unix)]
    let socket_path = Path::new(env::var("XDG_RUNTIME_DIR")?.as_str()).join("lan-mouse-socket.sock");
    Ok(thread::Builder::new()
        .name("cli-frontend".to_string())
        .spawn(move || {
        loop {
            eprint!("lan-mouse > ");
            std::io::stderr().flush().unwrap();
            let mut buf = String::new();
            match std::io::stdin().read_line(&mut buf) {
                Ok(len) => {
                    if let Some(event) = parse_cmd(buf, len) {
                        #[cfg(unix)]
                        let Ok(mut stream) = UnixStream::connect(&socket_path) else {
                            log::error!("Could not connect to lan-mouse-socket");
                            continue;
                        };
                        #[cfg(windows)]
                        let Ok(mut stream) = TcpStream::connect("127.0.0.1:5252".parse::<SocketAddrV4>().unwrap()) else {
                            log::error!("Could not connect to lan-mouse-server");
                            continue;
                        };
                        let json = serde_json::to_string(&event).unwrap();
                        let bytes = json.as_bytes();
                        let len = bytes.len().to_ne_bytes();
                        if let Err(e) = stream.write(&len) {
                            log::error!("error sending message: {e}");
                        };
                        if let Err(e) = stream.write(bytes) {
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
    })?)
}

fn parse_cmd(s: String, len: usize) -> Option<FrontendEvent> {
    if len == 0 {
        return Some(FrontendEvent::Shutdown())
    }
    let mut l = s.split_whitespace();
    let cmd = l.next()?;
    match cmd {
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
        _ => {
            log::error!("unknown command: {s}");
            None
        }
    }
}
