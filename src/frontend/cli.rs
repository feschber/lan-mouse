use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tokio::{io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader}, task::LocalSet};

#[cfg(unix)]
use tokio::net::unix::{ReadHalf, WriteHalf};
#[cfg(windows)]
use tokio::net::tcp::{ReadHalf, WriteHalf};

use std::{fmt::Display, io, str::{FromStr, SplitWhitespace}};

use crate::{client::{ClientHandle, Position}, config::DEFAULT_PORT};

use super::{FrontendEvent, FrontendRequest};

enum CommandType {
    NoCommand,
    Connect,
    Disconnect,
    Activate,
    Deactivate,
    List,
    SetHost,
    SetPort,
}

#[derive(Debug)]
struct InvalidCommand {
    cmd: String,
}

impl Display for InvalidCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid command: \"{}\"", self.cmd)
    }
}

impl FromStr for CommandType {
    type Err = InvalidCommand;

    fn from_str(s: &str) -> std::prelude::v1::Result<Self, Self::Err> {
        match s {
            "connect" => Ok(Self::Connect),
            "disconnect" => Ok(Self::Disconnect),
            "activate" => Ok(Self::Activate),
            "deactivate" => Ok(Self::Deactivate),
            "list" => Ok(Self::List),
            "set-host" => Ok(Self::SetHost),
            "set-port" => Ok(Self::SetPort),
            _ => Err(InvalidCommand { cmd: s.to_string() })
        }
    }
}

enum Command {
    NoCommand,
    Connect(Position, String, Option<u16>),
    Disconnect(ClientHandle),
    Activate(ClientHandle),
    Deactivate(ClientHandle),
    List,
    SetHost(ClientHandle, String),
    SetPort(ClientHandle, Option<u16>),
}

enum CommandParseError {
    Usage(CommandType),
    Invalid(InvalidCommand),
}

impl Display for CommandParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(cmd) => write!(f, "usage: "),
            Self::Invalid(cmd) => write!(f, "{}", cmd),
        }
    }
}

impl FromStr for Command {
    type Err = CommandParseError;

    fn from_str(cmd: &str) -> Result<Self, Self::Err> {
        let mut args = cmd.split_whitespace();
        let cmd_type: CommandType = match args.next() {
            Some(c) => c.parse().map_err(|e| CommandParseError::Invalid(e)),
            None => Ok(CommandType::NoCommand),
        }?;
        match cmd_type {
            CommandType::NoCommand => Ok(Command::NoCommand),
            CommandType::Connect => parse_connect_cmd(args),
            CommandType::Disconnect => parse_disconnect_cmd(args),
            CommandType::Activate => parse_activate_cmd(args),
            CommandType::Deactivate => parse_deactivate_cmd(args),
            CommandType::List => Ok(Command::List),
            CommandType::SetHost => parse_set_host(args),
            CommandType::SetPort => parse_set_port(args),
        }
    }
}

#[async_trait]
impl Exec for Command {
    async fn execute(&self, rx: &mut ReadHalf<'_>, tx: &mut WriteHalf<'_>) -> io::Result<()> {
        match self {
            Command::NoCommand => {},
            Command::Connect(pos, host, port) => {
                let request = FrontendRequest::Create;
                send_request(tx, request).await?;
                loop {
                    let response = await_event(rx).await;
                    match response {
                        Err(_) => break,
                        Ok(FrontendEvent::Created(h, _, _)) => {
                            for request in [
                                FrontendRequest::UpdateHostname(h, Some(host.clone())),
                                FrontendRequest::UpdatePort(h, port.unwrap_or(DEFAULT_PORT)),
                                FrontendRequest::UpdatePosition(h, *pos),
                            ] {
                                send_request(tx, request).await?;
                            }
                        }
                        _ => continue,
                    }
                }

            },
            Command::Disconnect(_) => todo!(),
            Command::Activate(_) => todo!(),
            Command::Deactivate(_) => todo!(),
            Command::List => todo!(),
            Command::SetHost(_, _) => todo!(),
            Command::SetPort(_, _) => todo!(),
        }
        Ok(())
    }
}

async fn send_request(tx: &mut WriteHalf<'_>, request: FrontendRequest) -> io::Result<()> {
    let json = serde_json::to_string(&request).unwrap();
    let bytes = json.as_bytes();
    let len = bytes.len();
    tx.write_u64(len as u64).await?;
    tx.write_all(bytes).await?;
    Ok(())
}

async fn await_event(rx: &mut ReadHalf<'_>) -> Result<FrontendEvent> {
    let len = rx.read_u64().await?;
    let mut buf = vec![0u8; len as usize];
    rx.read_exact(&mut buf).await?;
    let event: FrontendEvent = serde_json::from_slice(&buf)?;
    Ok(event)
}

#[async_trait]
trait Exec {
    async fn execute(&self, rx: &mut ReadHalf<'_>, tx: &mut WriteHalf<'_>) -> io::Result<()>;
}

fn parse_connect_cmd(mut args: SplitWhitespace<'_>) -> Result<Command, CommandParseError> {
    const USAGE: CommandParseError = CommandParseError::Usage(CommandType::Connect);
    let pos = args.next().ok_or(USAGE)?.parse().map_err(|_| USAGE)?;
    let host = args.next().ok_or(USAGE)?.to_string();
    let port = args.next().map(|p| p.parse().ok()).flatten();
    Ok(Command::Connect(pos, host, port))
}

fn parse_disconnect_cmd(mut args: SplitWhitespace<'_>) -> Result<Command, CommandParseError> {
    const USAGE: CommandParseError = CommandParseError::Usage(CommandType::Disconnect);
    let id = args.next().ok_or(USAGE)?.parse().map_err(|_| USAGE)?;
    Ok(Command::Disconnect(id))
}

fn parse_activate_cmd(mut args: SplitWhitespace<'_>) -> Result<Command, CommandParseError> {
    const USAGE: CommandParseError = CommandParseError::Usage(CommandType::Activate);
    let id = args.next().ok_or(USAGE)?.parse().map_err(|_| USAGE)?;
    Ok(Command::Activate(id))
}

fn parse_deactivate_cmd(mut args: SplitWhitespace<'_>) -> Result<Command, CommandParseError> {
    const USAGE: CommandParseError = CommandParseError::Usage(CommandType::Deactivate);
    let id = args.next().ok_or(USAGE)?.parse().map_err(|_| USAGE)?;
    Ok(Command::Deactivate(id))
}

fn parse_set_host(mut args: SplitWhitespace<'_>) -> Result<Command, CommandParseError> {
    const USAGE: CommandParseError = CommandParseError::Usage(CommandType::SetHost);
    let id = args.next().ok_or(USAGE)?.parse().map_err(|_| USAGE)?;
    let host = args.next().ok_or(USAGE)?.parse().map_err(|_| USAGE)?;
    Ok(Command::SetHost(id, host))
}

fn parse_set_port(mut args: SplitWhitespace<'_>) -> Result<Command, CommandParseError> {
    const USAGE: CommandParseError = CommandParseError::Usage(CommandType::SetPort);
    let id = args.next().ok_or(USAGE)?.parse().map_err(|_| USAGE)?;
    let port = args.next().map(|p| p.parse().ok()).flatten();
    Ok(Command::SetPort(id, port))
}

pub fn run() -> Result<()> {
    let Ok(stream) = super::wait_for_service() else {
        return Err(anyhow!("Could not connect to lan-mouse-socket"));
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    runtime.block_on(LocalSet::new().run_until(async move {
        #[cfg(unix)]
        let mut stream = tokio::net::UnixStream::from_std(stream)?;
        #[cfg(windows)]
        let mut stream = tokio::net::TcpStream::from_std(stream)?;
        let (mut rx, mut tx) = stream.split();

        let stdin = tokio::io::stdin();
        let stdin = BufReader::new(stdin);
        let mut stdin = stdin.lines();
        loop {
            tokio::select! {
                line = stdin.next_line() => {
                    let Some(line) = line? else {
                        break;
                    };
                    let cmd: Command = match line.parse() {
                        Ok(cmd) => cmd,
                        Err(e) => {
                            eprintln!("{e}");
                            continue;
                        }
                    };
                    cmd.execute(&mut rx, &mut tx).await?;
                }
                event = await_event(&mut rx) => {
                    let event = event?;
                    eprintln!("{event:?}");
                }


            }
        }
        anyhow::Ok(())
    }))?;
    Ok(())

}

