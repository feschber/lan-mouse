use std::io::Result;
use std::str;

#[cfg(unix)]
use std::{env, path::{Path, PathBuf}};

use tokio::io::{AsyncReadExt, WriteHalf, AsyncWriteExt};
use tokio::io::ReadHalf;

#[cfg(unix)]
use tokio::net::UnixStream;
#[cfg(unix)]
use tokio::net::UnixListener;
#[cfg(windows)]
use tokio::net::TcpStream;
#[cfg(windows)]
use tokio::net::TcpListener;

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
    /// change the listen port (recreate udp listener)
    ChangePort(u16),
    /// remove a client
    DelClient(ClientHandle),
    /// request an enumertaion of all clients
    Enumerate(),
    /// service shutdown
    Shutdown(),
    /// update a client (hostname, port, position)
    UpdateClient(ClientHandle, Option<String>, u16, Position),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FrontendNotify {
    NotifyClientCreate(ClientHandle, Option<String>, u16, Position),
    NotifyClientUpdate(ClientHandle, Option<String>, u16, Position),
    NotifyClientDelete(ClientHandle),
    /// new port, reason of failure (if failed)
    NotifyPortChange(u16, Option<String>),
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
    #[cfg(unix)]
    tx_streams: Vec<WriteHalf<UnixStream>>,
    #[cfg(windows)]
    tx_streams: Vec<WriteHalf<TcpStream>>,
}

impl FrontendListener {
    pub async fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
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
        let listener = TcpListener::bind("127.0.0.1:5252").await?; // abuse tcp

        let adapter = Self {
            listener,
            #[cfg(unix)]
            socket_path,
            tx_streams: vec![],
        };

        Ok(adapter)
    }

    #[cfg(unix)]
    pub async fn accept(&mut self) -> Result<ReadHalf<UnixStream>> {
        log::trace!("frontend.accept()");

        let stream = self.listener.accept().await?.0;
        let (rx, tx) = tokio::io::split(stream);
        self.tx_streams.push(tx);
        Ok(rx)
    }

    #[cfg(windows)]
    pub async fn accept(&mut self) -> Result<ReadHalf<TcpStream>> {
        let stream = self.listener.accept().await?.0;
        let (rx, tx) = tokio::io::split(stream);
        self.tx_streams.push(tx);
        Ok(rx)
    }


    pub(crate) async fn notify_all(&mut self, notify: FrontendNotify) -> Result<()> {
        // encode event
        let json = serde_json::to_string(&notify).unwrap();
        let payload = json.as_bytes();
        let len = payload.len().to_be_bytes();
        log::debug!("json: {json}, len: {}", payload.len());

        // TODO do simultaneously
        for tx in self.tx_streams.iter_mut() {
            // write len + payload
            tx.write(&len).await?;
            tx.write(payload).await?;
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for FrontendListener {
    fn drop(&mut self) {
        log::debug!("remove socket: {:?}", self.socket_path);
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(unix)]
pub async fn read_event(stream: &mut ReadHalf<UnixStream>) -> Result<FrontendEvent> {
    let len = stream.read_u64().await?;
    assert!(len <= 256);
    let mut buf = [0u8; 256];
    stream.read_exact(&mut buf[..len as usize]).await?;
    Ok(serde_json::from_slice(&buf[..len as usize])?)
}

#[cfg(windows)]
pub async fn read_event(stream: &mut ReadHalf<TcpStream>) -> Result<FrontendEvent> {
    let len = stream.read_u64().await?;
    let mut buf = [0u8; 256];
    stream.read_exact(&mut buf[..len as usize]).await?;
    Ok(serde_json::from_slice(&buf[..len as usize])?)
}

