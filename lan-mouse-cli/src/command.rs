use std::{
    fmt::Display,
    str::{FromStr, SplitWhitespace},
};

use lan_mouse_ipc::{ClientHandle, Position};

pub(super) enum CommandType {
    NoCommand,
    Help,
    Connect,
    Disconnect,
    Activate,
    Deactivate,
    List,
    SetHost,
    SetPort,
}

#[derive(Debug)]
pub(super) struct InvalidCommand {
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
            "help" => Ok(Self::Help),
            _ => Err(InvalidCommand { cmd: s.to_string() }),
        }
    }
}

#[derive(Debug)]
pub(super) enum Command {
    None,
    Help,
    Connect(Position, String, Option<u16>),
    Disconnect(ClientHandle),
    Activate(ClientHandle),
    Deactivate(ClientHandle),
    List,
    SetHost(ClientHandle, String),
    SetPort(ClientHandle, Option<u16>),
}

impl CommandType {
    pub(super) fn usage(&self) -> &'static str {
        match self {
            CommandType::Help => "help",
            CommandType::NoCommand => "",
            CommandType::Connect => "connect left|right|top|bottom <host> [<port>]",
            CommandType::Disconnect => "disconnect <id>",
            CommandType::Activate => "activate <id>",
            CommandType::Deactivate => "deactivate <id>",
            CommandType::List => "list",
            CommandType::SetHost => "set-host <id> <host>",
            CommandType::SetPort => "set-port <id> <host>",
        }
    }
}

pub(super) enum CommandParseError {
    Usage(CommandType),
    Invalid(InvalidCommand),
}

impl Display for CommandParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(cmd) => write!(f, "usage: {}", cmd.usage()),
            Self::Invalid(cmd) => write!(f, "{}", cmd),
        }
    }
}

impl FromStr for Command {
    type Err = CommandParseError;

    fn from_str(cmd: &str) -> Result<Self, Self::Err> {
        let mut args = cmd.split_whitespace();
        let cmd_type: CommandType = match args.next() {
            Some(c) => c.parse().map_err(CommandParseError::Invalid),
            None => Ok(CommandType::NoCommand),
        }?;
        match cmd_type {
            CommandType::Help => Ok(Command::Help),
            CommandType::NoCommand => Ok(Command::None),
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

fn parse_connect_cmd(mut args: SplitWhitespace<'_>) -> Result<Command, CommandParseError> {
    const USAGE: CommandParseError = CommandParseError::Usage(CommandType::Connect);
    let pos = args.next().ok_or(USAGE)?.parse().map_err(|_| USAGE)?;
    let host = args.next().ok_or(USAGE)?.to_string();
    let port = args.next().and_then(|p| p.parse().ok());
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
    let port = args.next().and_then(|p| p.parse().ok());
    Ok(Command::SetPort(id, port))
}
