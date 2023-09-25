use std::collections::HashMap;
use std::io::{Read, Result, Write};
use std::str;

#[cfg(unix)]
use std::{env, path::{Path, PathBuf}};

use mio::Interest;
use mio::{Registry, Token, event::Source};

#[cfg(unix)]
use mio::net::UnixStream;
#[cfg(unix)]
use mio::net::UnixListener;
#[cfg(windows)]
use mio::net::TcpStream;
#[cfg(windows)]
use mio::net::TcpListener;

use serde::{Serialize, Deserialize};

use crate::client::{Position, ClientHandle, Client};

/// cli frontend
pub mod cli;

/// gtk frontend
#[cfg(all(unix, feature = "gtk"))]
pub mod gtk;

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub enum FrontendEvent {
    /// add a new client
    AddClient(Option<String>, u16, Position),
    /// activate/deactivate client
    ActivateClient(ClientHandle, bool),
    /// update a client (hostname, port, position)
    UpdateClient(ClientHandle, Option<String>, u16, Position),
    /// remove a client
    DelClient(ClientHandle),
    /// request an enumertaion of all clients
    Enumerate(),
    /// service shutdown
    Shutdown(),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FrontendNotify {
    NotifyClientCreate(ClientHandle, Option<String>, u16, Position),
    NotifyClientUpdate(ClientHandle, Option<String>, u16, Position),
    NotifyClientDelete(ClientHandle),
    Enumerate(Vec<(Client, bool)>),
    NotifyError(String),
}

pub struct FrontendListener {
    #[cfg(windows)]
    listener: TcpListener,
    #[cfg(unix)]
    listener: UnixListener,
    #[cfg(unix)]
    socket_path: PathBuf,
    frontend_connections: HashMap<Token, FrontendConnection>,
}

impl FrontendListener {
    pub fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        #[cfg(unix)]
        let socket_path = Path::new(env::var("XDG_RUNTIME_DIR")?.as_str()).join("lan-mouse-socket.sock");
        #[cfg(unix)]
        log::debug!("remove socket: {:?}", socket_path);
        #[cfg(unix)]
        if socket_path.exists() {
            std::fs::remove_file(&socket_path).unwrap();
        }
        #[cfg(unix)]
        let listener = UnixListener::bind(&socket_path)?;

        #[cfg(windows)]
        let listener = TcpListener::bind("127.0.0.1:5252".parse().unwrap())?; // abuse tcp

        let adapter = Self {
            listener,
            #[cfg(unix)]
            socket_path,
            frontend_connections: HashMap::new(),
        };

        Ok(adapter)
    }

    #[cfg(unix)]
    pub fn handle_incoming<F>(&mut self, register_frontend: F) -> Result<()>
    where F: Fn(&mut UnixStream, Interest) -> Result<Token> {
        let (mut stream, _) = self.listener.accept()?;
        let token = register_frontend(&mut stream, Interest::READABLE)?;
        let con = FrontendConnection::new(stream);
        self.frontend_connections.insert(token, con);
        Ok(())
    }

    #[cfg(windows)]
    pub fn handle_incoming<F>(&mut self, register_frontend: F) -> Result<()>
    where F: Fn(&mut TcpStream, Interest) -> Result<Token> {
        let (mut stream, _) = self.listener.accept()?;
        let token = register_frontend(&mut stream, Interest::READABLE)?;
        let con = FrontendConnection::new(stream);
        self.frontend_connections.insert(token, con);
        Ok(())
    }

    pub fn read_event(&mut self, token: Token) -> Result<Option<FrontendEvent>> {
        if let Some(con) = self.frontend_connections.get_mut(&token) {
            con.handle_event()
        } else {
            panic!("unknown token");
        }
    }

    pub(crate) fn notify_all(&mut self, notify: FrontendNotify) -> Result<()> {
        // encode event
        let json = serde_json::to_string(&notify).unwrap();
        let payload = json.as_bytes();
        let len = payload.len().to_ne_bytes();
        log::debug!("json: {json}, len: {}", payload.len());

        for con in self.frontend_connections.values_mut() {
            // write len + payload
            con.stream.write(&len)?;
            con.stream.write(payload)?;
        }
        Ok(())
    }
}

impl Source for FrontendListener {
    fn register(
        &mut self,
        registry: &Registry,
        token: Token,
        interests: mio::Interest,
    ) -> Result<()> {
        self.listener.register(registry, token, interests)
    }

    fn reregister(
        &mut self,
        registry: &Registry,
        token: Token,
        interests: mio::Interest,
    ) -> Result<()> {
        self.listener.reregister(registry, token, interests)
    }

    fn deregister(&mut self, registry: &Registry) -> Result<()> {
        self.listener.deregister(registry)
    }
}

#[cfg(unix)]
impl Drop for FrontendListener {
    fn drop(&mut self) {
        log::debug!("remove socket: {:?}", self.socket_path);
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

enum ReceiveState {
    Len, Data,
}

pub struct FrontendConnection {
    #[cfg(unix)]
    stream: UnixStream,
    #[cfg(windows)]
    stream: TcpStream,
    state: ReceiveState,
    len: usize,
    len_buf: [u8; std::mem::size_of::<usize>()],
    recieve_buf: [u8; 256], // FIXME
    pos: usize,
}

impl FrontendConnection {
    #[cfg(unix)]
    pub fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            state: ReceiveState::Len,
            len: 0,
            len_buf: [0u8; std::mem::size_of::<usize>()],
            recieve_buf: [0u8; 256],
            pos: 0,
        }
    }

    #[cfg(windows)]
    pub fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            state: ReceiveState::Len,
            len: 0,
            len_buf: [0u8; std::mem::size_of::<usize>()],
            recieve_buf: [0u8; 256],
            pos: 0,
        }
    }

    pub fn handle_event(&mut self) -> Result<Option<FrontendEvent>> {
        match self.state {
            ReceiveState::Len => {
                // we receive sizeof(usize) Bytes
                let n = self.stream.read(&mut self.len_buf)?;
                self.pos += n;
                if self.pos == self.len_buf.len() {
                    self.state = ReceiveState::Data;
                    self.len = usize::from_ne_bytes(self.len_buf);
                    self.pos = 0;
                }
                Ok(None)
            },
            ReceiveState::Data => {
                // read at most as many bytes as the length of the next event
                let n = self.stream.read(&mut self.recieve_buf[..self.len])?;
                self.pos += n;
                if n == self.len {
                    self.state = ReceiveState::Len;
                    self.pos = 0;
                    Ok(Some(serde_json::from_slice(&self.recieve_buf[..self.len])?))
                } else {
                    Ok(None)
                }
            }
        }
    }
}
