use std::io::{Read, Result};
use std::{str, net::SocketAddr};

#[cfg(unix)]
use std::{env, path::{Path, PathBuf}};

use mio::{Registry, Token, event::Source};

#[cfg(unix)]
use mio::net::UnixListener;
#[cfg(windows)]
use mio::net::TcpListener;

use serde::{Serialize, Deserialize};

use crate::client::{Client, Position};

/// cli frontend
pub mod cli;

/// gtk frontend
#[cfg(all(unix, feature = "gtk"))]
pub mod gtk;

#[derive(Debug, Eq, PartialEq, Clone, Serialize, Deserialize)]
pub enum FrontendEvent {
    RequestPortChange(u16),
    RequestClientAdd(SocketAddr, Position),
    RequestClientDelete(Client),
    RequestClientUpdate(Client),
    RequestShutdown(),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FrontendNotify {
    NotifyClientCreate(Client),
    NotifyError(String),
}

pub struct FrontendAdapter {
    #[cfg(windows)]
    listener: TcpListener,
    #[cfg(unix)]
    listener: UnixListener,
    #[cfg(unix)]
    socket_path: PathBuf,
}

impl FrontendAdapter {
    pub fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        #[cfg(unix)]
        let socket_path = Path::new(env::var("XDG_RUNTIME_DIR")?.as_str()).join("lan-mouse-socket.sock");
        #[cfg(unix)]
        let listener = UnixListener::bind(&socket_path)?;

        #[cfg(windows)]
        let listener = TcpListener::bind("127.0.0.1:5252".parse().unwrap())?; // abuse tcp

        let adapter = Self {
            listener,
            #[cfg(unix)]
            socket_path,
        };

        Ok(adapter)
    }

    pub fn read_event(&mut self) -> Result<FrontendEvent>{
        let (mut stream, _) = self.listener.accept()?;
        let mut buf = [0u8; 128]; // FIXME
        stream.read(&mut buf)?;
        let json = str::from_utf8(&buf)
            .unwrap()
            .trim_end_matches(char::from(0)); // remove trailing 0-bytes
        let event = serde_json::from_str(json).unwrap();
        log::debug!("{:?}", event);
        Ok(event)
    }

    pub fn notify(&self, _event: FrontendNotify) { }
}

impl Source for FrontendAdapter {
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
impl Drop for FrontendAdapter {
    fn drop(&mut self) {
        log::debug!("remove socket: {:?}", self.socket_path);
        std::fs::remove_file(&self.socket_path).unwrap();
    }
}

pub trait Frontend { }
