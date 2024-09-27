use futures::StreamExt;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    task::LocalSet,
};

use std::io::{self, Write};

use self::command::{Command, CommandType};

use lan_mouse_ipc::{
    AsyncFrontendEventReader, AsyncFrontendRequestWriter, ClientConfig, ClientHandle, ClientState,
    FrontendEvent, FrontendRequest, IpcError, DEFAULT_PORT,
};

mod command;

pub fn run() -> Result<(), IpcError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    runtime.block_on(LocalSet::new().run_until(async move {
        let (rx, tx) = lan_mouse_ipc::connect_async().await?;
        let mut cli = Cli::new(rx, tx);
        cli.run().await
    }))?;
    Ok(())
}

struct Cli {
    clients: Vec<(ClientHandle, ClientConfig, ClientState)>,
    changed: Option<ClientHandle>,
    rx: AsyncFrontendEventReader,
    tx: AsyncFrontendRequestWriter,
}

impl Cli {
    fn new(rx: AsyncFrontendEventReader, tx: AsyncFrontendRequestWriter) -> Cli {
        Self {
            clients: vec![],
            changed: None,
            rx,
            tx,
        }
    }

    async fn run(&mut self) -> Result<(), IpcError> {
        let stdin = tokio::io::stdin();
        let stdin = BufReader::new(stdin);
        let mut stdin = stdin.lines();

        /* initial state sync */
        self.clients = loop {
            match self.rx.next().await {
                Some(Ok(e)) => {
                    if let FrontendEvent::Enumerate(clients) = e {
                        break clients;
                    }
                }
                Some(Err(e)) => return Err(e),
                None => return Ok(()),
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
                event = self.rx.next() => {
                    if let Some(event) = event {
                        self.handle_event(event?);
                    } else {
                        break Ok(());
                    }
                }
            }
            if let Some(handle) = self.changed.take() {
                self.update_client(handle).await?;
            }
        }
    }

    async fn update_client(&mut self, handle: ClientHandle) -> Result<(), IpcError> {
        self.tx.request(FrontendRequest::GetState(handle)).await?;
        while let Some(Ok(event)) = self.rx.next().await {
            self.handle_event(event.clone());
            if let FrontendEvent::State(_, _, _) | FrontendEvent::NoSuchClient(_) = event {
                break;
            }
        }
        Ok(())
    }

    async fn execute(&mut self, cmd: Command) -> Result<(), IpcError> {
        match cmd {
            Command::None => {}
            Command::Connect(pos, host, port) => {
                let request = FrontendRequest::Create;
                self.tx.request(request).await?;
                let handle = loop {
                    if let Some(Ok(event)) = self.rx.next().await {
                        match event {
                            FrontendEvent::Created(h, c, s) => {
                                self.clients.push((h, c, s));
                                break h;
                            }
                            _ => {
                                self.handle_event(event);
                                continue;
                            }
                        }
                    }
                };
                for request in [
                    FrontendRequest::UpdateHostname(handle, Some(host.clone())),
                    FrontendRequest::UpdatePort(handle, port.unwrap_or(DEFAULT_PORT)),
                    FrontendRequest::UpdatePosition(handle, pos),
                ] {
                    self.tx.request(request).await?;
                }
                self.update_client(handle).await?;
            }
            Command::Disconnect(id) => {
                self.tx.request(FrontendRequest::Delete(id)).await?;
                loop {
                    if let Some(Ok(event)) = self.rx.next().await {
                        self.handle_event(event.clone());
                        if let FrontendEvent::Deleted(_) = event {
                            self.handle_event(event);
                            break;
                        }
                    }
                }
            }
            Command::Activate(id) => {
                self.tx.request(FrontendRequest::Activate(id, true)).await?;
                self.update_client(id).await?;
            }
            Command::Deactivate(id) => {
                self.tx
                    .request(FrontendRequest::Activate(id, false))
                    .await?;
                self.update_client(id).await?;
            }
            Command::List => {
                self.tx.request(FrontendRequest::Enumerate()).await?;
                while let Some(e) = self.rx.next().await {
                    let event = e?;
                    self.handle_event(event.clone());
                    if let FrontendEvent::Enumerate(_) = event {
                        break;
                    }
                }
            }
            Command::SetHost(handle, host) => {
                let request = FrontendRequest::UpdateHostname(handle, Some(host.clone()));
                self.tx.request(request).await?;
                self.update_client(handle).await?;
            }
            Command::SetPort(handle, port) => {
                let request = FrontendRequest::UpdatePort(handle, port.unwrap_or(DEFAULT_PORT));
                self.tx.request(request).await?;
                self.update_client(handle).await?;
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

    fn find_mut(
        &mut self,
        handle: ClientHandle,
    ) -> Option<&mut (ClientHandle, ClientConfig, ClientState)> {
        self.clients.iter_mut().find(|(h, _, _)| *h == handle)
    }

    fn remove(
        &mut self,
        handle: ClientHandle,
    ) -> Option<(ClientHandle, ClientConfig, ClientState)> {
        let idx = self.clients.iter().position(|(h, _, _)| *h == handle);
        idx.map(|i| self.clients.swap_remove(i))
    }

    fn handle_event(&mut self, event: FrontendEvent) {
        match event {
            FrontendEvent::Changed(h) => self.changed = Some(h),
            FrontendEvent::Created(h, c, s) => {
                eprint!("client added ({h}): ");
                print_config(&c);
                eprint!(" ");
                print_state(&s);
                eprintln!();
                self.clients.push((h, c, s));
            }
            FrontendEvent::NoSuchClient(h) => {
                eprintln!("no such client: {h}");
            }
            FrontendEvent::State(h, c, s) => {
                if let Some((_, config, state)) = self.find_mut(h) {
                    let old_host = config.hostname.clone().unwrap_or("\"\"".into());
                    let new_host = c.hostname.clone().unwrap_or("\"\"".into());
                    if old_host != new_host {
                        eprintln!(
                            "client {h}: hostname updated ({} -> {})",
                            old_host, new_host
                        );
                    }
                    if config.port != c.port {
                        eprintln!("client {h} changed port: {} -> {}", config.port, c.port);
                    }
                    if config.fix_ips != c.fix_ips {
                        eprintln!("client {h} ips updated: {:?}", c.fix_ips)
                    }
                    *config = c;
                    if state.active ^ s.active {
                        eprintln!(
                            "client {h} {}",
                            if s.active { "activated" } else { "deactivated" }
                        );
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
                self.clients = clients;
                self.print_clients();
            }
            FrontendEvent::Error(e) => {
                eprintln!("ERROR: {e}");
            }
            FrontendEvent::CaptureStatus(s) => {
                eprintln!("capture status: {s:?}")
            }
            FrontendEvent::EmulationStatus(s) => {
                eprintln!("emulation status: {s:?}")
            }
            FrontendEvent::AuthorizedUpdated(keys) => {
                eprintln!("authorized keys changed:");
                for key in keys {
                    eprintln!("{key}");
                }
            }
        }
    }

    fn print_clients(&mut self) {
        for (h, c, s) in self.clients.iter() {
            eprint!("client {h}: ");
            print_config(c);
            eprint!(" ");
            print_state(s);
            eprintln!();
        }
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
