use anyhow::{anyhow, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    task::LocalSet,
};

#[cfg(windows)]
use tokio::net::tcp::{ReadHalf, WriteHalf};
#[cfg(unix)]
use tokio::net::unix::{ReadHalf, WriteHalf};

use std::{collections::HashMap, io::{self, Write}};

use crate::{
    client::{ClientConfig, ClientHandle, ClientState},
    config::DEFAULT_PORT,
};

use self::command::{Command, CommandType};

use super::{FrontendEvent, FrontendRequest};

mod command;

pub fn run() -> Result<()> {
    let Ok(stream) = super::wait_for_service() else {
        return Err(anyhow!("Could not connect to lan-mouse-socket"));
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    runtime.block_on(LocalSet::new().run_until(async move {
        stream.set_nonblocking(true)?;
        #[cfg(unix)]
        let mut stream = tokio::net::UnixStream::from_std(stream)?;
        #[cfg(windows)]
        let mut stream = tokio::net::TcpStream::from_std(stream)?;
        let (rx, tx) = stream.split();

        let mut cli = Cli::new(rx, tx);
        cli.run().await
    }))?;
    Ok(())
}

struct Cli<'a> {
    clients: Vec<(ClientHandle, ClientConfig, ClientState)>,
    rx: ReadHalf<'a>,
    tx: WriteHalf<'a>,
}

impl<'a> Cli<'a> {
    fn new(rx: ReadHalf<'a>, tx: WriteHalf<'a>) -> Cli<'a> {
        Self { clients: vec![], rx, tx }
    }

    async fn run(&mut self) -> Result<()> {
        let stdin = tokio::io::stdin();
        let stdin = BufReader::new(stdin);
        let mut stdin = stdin.lines();

        /* initial state sync */
        let request = FrontendRequest::Enumerate();
        self.send_request(request).await?;

        self.clients = loop {
            let event = self.await_event().await?;
            if let FrontendEvent::Enumerate(clients) = event {
                break clients;
            }
        };

        loop {
            prompt()?;
            tokio::select! {
                line = stdin.next_line() => {
                    let Some(line) = line? else {
                        break Ok(());
                    };
                    let cmd: Command = match line.parse() {
                        Ok(cmd) => cmd,
                        Err(e) => {
                            eprintln!("{e}");
                            continue;
                        }
                    };
                    self.execute(cmd).await?;
                }
                event = self.await_event() => {
                    let event = event?;
                    self.handle_event(event);
                }
            }
        }
    }

    async fn execute(&mut self, cmd: Command) -> Result<()> {
        match cmd {
            Command::None => {}
            Command::Connect(pos, host, port) => {
                let request = FrontendRequest::Create;
                self.send_request(request).await?;
                let handle = loop {
                    match self.await_event().await? {
                        FrontendEvent::Created(h, _, _) => break h,
                        _ => continue,
                    }
                };
                for request in [
                    FrontendRequest::UpdateHostname(handle, Some(host.clone())),
                    FrontendRequest::UpdatePort(handle, port.unwrap_or(DEFAULT_PORT)),
                    FrontendRequest::UpdatePosition(handle, pos),
                ] {
                    self.send_request(request).await?;
                    self.await_event().await?;
                }
            }
            Command::Disconnect(id) => {
                self.send_request(FrontendRequest::Delete(id)).await?;
                while let Ok(response) = self.await_event().await {
                    if let FrontendEvent::Deleted(h) = response {
                        if h == id {
                            eprintln!("removed client {h}");
                            break;
                        }
                    }
                }
            }
            Command::Activate(id) => {
                self.send_request(FrontendRequest::Activate(id, true)).await?;
                while let Ok(response) = self.await_event().await {
                    if let FrontendEvent::StateChange(h, s) = response {
                        if h == id {
                            eprintln!(
                                "client {h} {}",
                                if s.active { "activated" } else { "deactivated" }
                            );
                            break;
                        }
                    }
                }
            }
            Command::Deactivate(id) => {
                self.send_request(FrontendRequest::Activate(id, false)).await?;
                while let Ok(response) = self.await_event().await {
                    if let FrontendEvent::StateChange(h, s) = response {
                        if h == id {
                            eprintln!(
                                "client {h} {}",
                                if s.active { "activated" } else { "deactivated" }
                            );
                            break;
                        }
                    }
                }
            }
            Command::List => {
                self.send_request(FrontendRequest::Enumerate()).await?;
                while let Ok(response) = self.await_event().await {
                    if let FrontendEvent::Enumerate(clients) = response {
                        for (h, c, s) in clients {
                            eprint!("client {h}: ");
                            print_config(&c);
                            eprint!(" ");
                            print_state(&s);
                            eprintln!();
                        }
                        break;
                    }
                }
            }
            Command::SetHost(handle, host) => {
                let request = FrontendRequest::UpdateHostname(handle, Some(host.clone()));
                self.send_request(request).await?;
                while let Ok(event) = self.await_event().await {
                    if let FrontendEvent::Updated(h, c) = event {
                        if h == handle {
                            eprintln!(
                                "changed hostname: {}",
                                c.hostname.unwrap_or("no hostname".into())
                            );
                            break;
                        }
                    }
                }
            }
            Command::SetPort(handle, port) => {
                let request = FrontendRequest::UpdatePort(handle, port.unwrap_or(DEFAULT_PORT));
                self.send_request(request).await?;
                while let Ok(event) = self.await_event().await {
                    if let FrontendEvent::Updated(h, c) = event {
                        eprintln!("client {h} changed port: {}", c.port);
                        break;
                    }
                }
            }
            Command::Help => {
                for cmd_type in [
                    CommandType::List,
                    CommandType::Connect,
                    CommandType::Disconnect,
                    CommandType::Activate,
                    CommandType::Deactivate,
                    CommandType::SetHost,
                    CommandType::SetPort,
                ] {
                    eprintln!("{}", cmd_type.usage());
                }
            }
        }
        Ok(())
    }

    fn find_mut(&mut self, handle: ClientHandle) -> Option<&mut (ClientHandle, ClientConfig, ClientState)> {
        self.clients.iter_mut().find(|(h,_,_)| *h == handle)
    }

    fn remove(&mut self, handle: ClientHandle) -> Option<(ClientHandle, ClientConfig, ClientState)> {
        let idx = self.clients.iter().position(|(h,_,_)| *h == handle);
        idx.map(|i| self.clients.swap_remove(i))
    }

    fn handle_event(&mut self, event: FrontendEvent) {
        eprintln!();
        match event {
            FrontendEvent::Created(h, c, s) => {
                eprint!("client added ({h}): ");
                print_config(&c);
                eprint!(" ");
                print_state(&s);
                eprintln!();
                self.clients.push((h, c, s));
            }
            FrontendEvent::Updated(h, c) => {
                if let Some((_,config,_)) = self.find_mut(h) {
                    let old_host = config.hostname.clone().unwrap_or("\"\"".into());
                    let new_host = c.hostname.clone().unwrap_or("\"\"".into());
                    if old_host != new_host {
                        eprintln!("client {h}: hostname updated ({} -> {})", old_host, new_host);
                    }
                    if config.port != c.port {
                        eprintln!("client {h} changed port: {} -> {}", config.port, c.port);
                    }
                    if config.fix_ips != c.fix_ips {
                        eprintln!("client {h} ips updated: {:?}", c.fix_ips)
                    }
                    *config = c;
                }
            }
            FrontendEvent::StateChange(h, s) => {
                if let Some((_,_,state)) = self.find_mut(h) {
                    if state.active ^ s.active {
                        eprintln!("client {h} {}", if s.active { "activated" } else { "deactivated" });
                    }
                    *state = s;
                }
            }
            FrontendEvent::Deleted(h) => {
                if let Some((h, c, _)) = self.remove(h) {
                    eprint!("client {h} removed (");
                    print_config(&c);
                    eprintln!(")");
                }
            }
            FrontendEvent::PortChanged(p, e) => {
                if let Some(e) = e {
                    eprintln!("failed to change port: {e}");
                } else {
                    eprintln!("changed port to {p}");
                }
            }
            FrontendEvent::Enumerate(clients) => {
                for (h, c, s) in clients.iter() {
                    eprint!("client {h}: ");
                    print_config(&c);
                    eprint!(" ");
                    print_state(&s);
                    eprintln!();
                }
                self.clients = clients;
            }
            FrontendEvent::Error(e) => {
                eprintln!("ERROR: {e}");
            }
        }
    }

    async fn send_request(&mut self, request: FrontendRequest) -> io::Result<()> {
        let json = serde_json::to_string(&request).unwrap();
        let bytes = json.as_bytes();
        let len = bytes.len();
        self.tx.write_u64(len as u64).await?;
        self.tx.write_all(bytes).await?;
        Ok(())
    }

    async fn await_event(&mut self) -> Result<FrontendEvent> {
        let len = self.rx.read_u64().await?;
        let mut buf = vec![0u8; len as usize];
        self.rx.read_exact(&mut buf).await?;
        let event: FrontendEvent = serde_json::from_slice(&buf)?;
        Ok(event)
    }
}

fn prompt() -> io::Result<()> {
    eprint!("lan-mouse > ");
    std::io::stderr().flush()?;
    Ok(())
}

fn print_config(c: &ClientConfig) {
    eprint!(
        "{}:{} ({}), ips: {:?}",
        c.hostname.clone().unwrap_or("(no hostname)".into()),
        c.port,
        c.pos,
        c.fix_ips
    );
}

fn print_state(s: &ClientState) {
    eprint!("active: {}, dns: {:?}", s.active, s.ips);
}
