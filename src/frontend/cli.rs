use anyhow::{anyhow, Result, Context};
use std::{thread, io::{Write, Read, ErrorKind}, str::SplitWhitespace};
#[cfg(windows)]
use std::net::SocketAddrV4;

#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(windows)]
use std::net::TcpStream;

use crate::{client::Position, config::DEFAULT_PORT};

use super::{FrontendEvent, FrontendNotify};

pub fn run() -> Result<()> {
    #[cfg(unix)]
    let socket_path = super::FrontendListener::socket_path()?;

    #[cfg(unix)]
    let Ok(mut tx) = UnixStream::connect(&socket_path) else {
        return Err(anyhow!("Could not connect to lan-mouse-socket"));
    };

    #[cfg(windows)]
    let Ok(mut tx) = TcpStream::connect("127.0.0.1:5252".parse::<SocketAddrV4>().unwrap()) else {
        return Err(anyhow!("Could not connect to lan-mouse-socket"));
    };

    let mut rx = tx.try_clone()?;

    let reader = thread::Builder::new()
        .name("cli-frontend".to_string())
        .spawn(move || {
        // all further prompts
        prompt();
        loop {
            let mut buf = String::new();
            match std::io::stdin().read_line(&mut buf) {
                Ok(0) => break,
                Ok(len) => {
                    if let Some(events) = parse_cmd(buf, len) {
                        for event in events.iter() {
                            let json = serde_json::to_string(&event).unwrap();
                            let bytes = json.as_bytes();
                            let len = bytes.len().to_be_bytes();
                            if let Err(e) = tx.write(&len) {
                                log::error!("error sending message: {e}");
                            };
                            if let Err(e) = tx.write(bytes) {
                                log::error!("error sending message: {e}");
                            };
                            if *event == FrontendEvent::Shutdown() {
                                return;
                            }
                        }
                        // prompt is printed after the server response is received
                    } else {
                        prompt();
                    }
                }
                Err(e) => {
                    log::error!("error reading from stdin: {e}");
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
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
                    Err(e) => break log::error!("{e}"),
                };
                let len = usize::from_be_bytes(len);

                // read payload
                let mut buf: Vec<u8> = vec![0u8; len];
                match rx.read_exact(&mut buf[..len]) {
                    Ok(()) => (),
                    Err(e) if e.kind() == ErrorKind::UnexpectedEof => break,
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
                    FrontendNotify::Enumerate(clients) => {
                        for (client, active) in clients.into_iter() {
                            log::info!("client ({}) [{}]: active: {}, associated addresses: [{}]",
                                client.handle,
                                client.hostname.as_deref().unwrap_or(""),
                                if active { "yes" } else { "no" },
                                client.addrs.into_iter().map(|a| a.to_string())
                                .collect::<Vec<String>>()
                                .join(", ")
                            );
                        }
                    },
                    FrontendNotify::NotifyPortChange(port, msg) => {
                        match msg {
                            Some(msg) => log::info!("could not change port: {msg}"),
                            None => log::info!("port changed: {port}"),
                        }
                    }
                }
                prompt();
            }
        })?;
    match reader.join() {
        Ok(_) => (),
        Err(e) => {
            let msg = match (e.downcast_ref::<&str>(), e.downcast_ref::<String>()) {
                (Some(&s), _) => s,
                (_, Some(s)) => s,
                _ => "no panic info"
            };
            log::error!("reader thread paniced: {msg}");
        },
    }
    match writer.join() {
        Ok(_) => (),
        Err(e) => {
            let msg = match (e.downcast_ref::<&str>(), e.downcast_ref::<String>()) {
                (Some(&s), _) => s,
                (_, Some(s)) => s,
                _ => "no panic info"
            };
            log::error!("writer thread paniced: {msg}");
        },
    }
    Ok(())
}

fn prompt() {
    eprint!("lan-mouse > ");
    std::io::stderr().flush().unwrap();
}

fn parse_cmd(s: String, len: usize) -> Option<Vec<FrontendEvent>> {
    if len == 0 {
        return Some(vec![FrontendEvent::Shutdown()])
    }
    let mut l = s.split_whitespace();
    let cmd = l.next()?;
    let res = match cmd {
        "help" => {
            log::info!("list                                                 list clients");
            log::info!("connect <host> left|right|top|bottom [port]          add a new client");
            log::info!("disconnect <client>                                  remove a client");
            log::info!("activate <client>                                    activate a client");
            log::info!("deactivate <client>                                  deactivate a client");
            log::info!("exit                                                 exit lan-mouse");
            log::info!("setport <port>                                       change port");
            None
        }
        "exit" => return Some(vec![FrontendEvent::Shutdown()]),
        "list" => return Some(vec![FrontendEvent::Enumerate()]),
        "connect" => Some(parse_connect(l)),
        "disconnect" => Some(parse_disconnect(l)),
        "activate" => Some(parse_activate(l)),
        "deactivate" => Some(parse_deactivate(l)),
        "setport" => Some(parse_port(l)),
        _ => {
            log::error!("unknown command: {s}");
            None
        }
    };
    match res {
        Some(Ok(e)) => Some(e),
        Some(Err(e)) => {
            log::warn!("{e}");
            None
        }
        _ => None
    }
}

fn parse_connect(mut l: SplitWhitespace) -> Result<Vec<FrontendEvent>> {
    let usage = "usage: connect <host> left|right|top|bottom [port]";
    let host = l.next().context(usage)?.to_owned();
    let pos = match l.next().context(usage)? {
        "right" => Position::Right,
        "top" => Position::Top,
        "bottom" => Position::Bottom,
        _ => Position::Left,
    };
    let port = if let Some(p) = l.next() {
        p.parse()?
    } else {
        DEFAULT_PORT
    };
    Ok(vec![FrontendEvent::AddClient(Some(host), port, pos), FrontendEvent::Enumerate()])
}

fn parse_disconnect(mut l: SplitWhitespace) -> Result<Vec<FrontendEvent>> {
    let client = l.next().context("usage: disconnect <client_id>")?.parse()?;
    Ok(vec![FrontendEvent::DelClient(client), FrontendEvent::Enumerate()])
}

fn parse_activate(mut l: SplitWhitespace) -> Result<Vec<FrontendEvent>> {
    let client = l.next().context("usage: activate <client_id>")?.parse()?;
    Ok(vec![FrontendEvent::ActivateClient(client, true), FrontendEvent::Enumerate()])
}

fn parse_deactivate(mut l: SplitWhitespace) -> Result<Vec<FrontendEvent>> {
    let client = l.next().context("usage: deactivate <client_id>")?.parse()?;
    Ok(vec![FrontendEvent::ActivateClient(client, false), FrontendEvent::Enumerate()])
}

fn parse_port(mut l: SplitWhitespace) -> Result<Vec<FrontendEvent>> {
    let port = l.next().context("usage: setport <port>")?.parse()?;
    Ok(vec![FrontendEvent::ChangePort(port)])
}
