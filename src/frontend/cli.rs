use anyhow::{anyhow, Context, Result};

use std::{
    io::{ErrorKind, Read, Write},
    str::SplitWhitespace,
    thread,
};

use crate::{client::Position, config::DEFAULT_PORT};

use super::{FrontendEvent, FrontendNotify};

pub fn run() -> Result<()> {
    let Ok(mut tx) = super::wait_for_service() else {
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
                    Ok(0) => return,
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
                        if e.kind() != ErrorKind::UnexpectedEof {
                            log::error!("error reading from stdin: {e}");
                        }
                        return;
                    }
                }
            }
        })?;

    let _ = thread::Builder::new()
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
                    FrontendNotify::NotifyClientActivate(handle, active) => {
                        if active {
                            log::info!("client {handle} activated");
                        } else {
                            log::info!("client {handle} deactivated");
                        }
                    }
                    FrontendNotify::NotifyClientCreate(client) => {
                        let handle = client.handle;
                        let port = client.port;
                        let pos = client.pos;
                        let hostname = client.hostname.as_deref().unwrap_or("");
                        log::info!("new client ({handle}): {hostname}:{port} - {pos}");
                    }
                    FrontendNotify::NotifyClientUpdate(client) => {
                        let handle = client.handle;
                        let port = client.port;
                        let pos = client.pos;
                        let hostname = client.hostname.as_deref().unwrap_or("");
                        log::info!("client ({handle}) updated: {hostname}:{port} - {pos}");
                    }
                    FrontendNotify::NotifyClientDelete(client) => {
                        log::info!("client ({client}) deleted.");
                    }
                    FrontendNotify::NotifyError(e) => {
                        log::warn!("{e}");
                    }
                    FrontendNotify::Enumerate(clients) => {
                        for (client, active) in clients.into_iter() {
                            log::info!(
                                "client ({}) [{}]: active: {}, associated addresses: [{}]",
                                client.handle,
                                client.hostname.as_deref().unwrap_or(""),
                                if active { "yes" } else { "no" },
                                client
                                    .ips
                                    .into_iter()
                                    .map(|a| a.to_string())
                                    .collect::<Vec<String>>()
                                    .join(", ")
                            );
                        }
                    }
                    FrontendNotify::NotifyPortChange(port, msg) => match msg {
                        Some(msg) => log::info!("could not change port: {msg}"),
                        None => log::info!("port changed: {port}"),
                    },
                }
                prompt();
            }
        })?;
    match reader.join() {
        Ok(_) => {}
        Err(e) => {
            let msg = match (e.downcast_ref::<&str>(), e.downcast_ref::<String>()) {
                (Some(&s), _) => s,
                (_, Some(s)) => s,
                _ => "no panic info",
            };
            log::error!("reader thread paniced: {msg}");
        }
    }
    Ok(())
}

fn prompt() {
    eprint!("lan-mouse > ");
    std::io::stderr().flush().unwrap();
}

fn parse_cmd(s: String, len: usize) -> Option<Vec<FrontendEvent>> {
    if len == 0 {
        return Some(vec![FrontendEvent::Shutdown()]);
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
        _ => None,
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
    Ok(vec![
        FrontendEvent::AddClient(Some(host), port, pos),
        FrontendEvent::Enumerate(),
    ])
}

fn parse_disconnect(mut l: SplitWhitespace) -> Result<Vec<FrontendEvent>> {
    let client = l.next().context("usage: disconnect <client_id>")?.parse()?;
    Ok(vec![
        FrontendEvent::DelClient(client),
        FrontendEvent::Enumerate(),
    ])
}

fn parse_activate(mut l: SplitWhitespace) -> Result<Vec<FrontendEvent>> {
    let client = l.next().context("usage: activate <client_id>")?.parse()?;
    Ok(vec![
        FrontendEvent::ActivateClient(client, true),
        FrontendEvent::Enumerate(),
    ])
}

fn parse_deactivate(mut l: SplitWhitespace) -> Result<Vec<FrontendEvent>> {
    let client = l.next().context("usage: deactivate <client_id>")?.parse()?;
    Ok(vec![
        FrontendEvent::ActivateClient(client, false),
        FrontendEvent::Enumerate(),
    ])
}

fn parse_port(mut l: SplitWhitespace) -> Result<Vec<FrontendEvent>> {
    let port = l.next().context("usage: setport <port>")?.parse()?;
    Ok(vec![FrontendEvent::ChangePort(port)])
}
