use crate::server::Server;
use lan_mouse_ipc::{ClientHandle, DEFAULT_PORT};
use lan_mouse_proto::{ProtoEvent, MAX_EVENT_SIZE};
use std::{
    collections::{HashMap, HashSet},
    io,
    net::SocketAddr,
    rc::Rc,
    sync::Arc,
};
use thiserror::Error;
use tokio::{
    net::UdpSocket,
    sync::Mutex,
    task::{spawn_local, JoinSet},
};
use webrtc_dtls::{
    config::{Config, ExtendedMasterSecretType},
    conn::DTLSConn,
    crypto::Certificate,
};
use webrtc_util::Conn;

#[derive(Debug, Error)]
pub(crate) enum LanMouseConnectionError {
    #[error(transparent)]
    Bind(#[from] io::Error),
    #[error(transparent)]
    Dtls(#[from] webrtc_dtls::Error),
    #[error(transparent)]
    Webrtc(#[from] webrtc_util::Error),
    #[error("not connected")]
    NotConnected,
}

async fn connect(
    addr: SocketAddr,
) -> Result<(Arc<dyn Conn + Sync + Send>, SocketAddr), LanMouseConnectionError> {
    let conn = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    conn.connect(addr).await?;
    log::info!("connected to {addr}, establishing secure dtls channel ...");
    let certificate = Certificate::generate_self_signed(["localhost".to_owned()])?;
    let config = Config {
        certificates: vec![certificate],
        insecure_skip_verify: true,
        extended_master_secret: ExtendedMasterSecretType::Require,
        ..Default::default()
    };
    let dtls_conn: Arc<dyn Conn + Send + Sync> =
        Arc::new(DTLSConn::new(conn, config, true, None).await?);
    Ok((dtls_conn, addr))
}

async fn connect_any(
    addrs: &[SocketAddr],
) -> Result<(Arc<dyn Conn + Send + Sync>, SocketAddr), LanMouseConnectionError> {
    let mut joinset = JoinSet::new();
    for &addr in addrs {
        joinset.spawn_local(connect(addr));
    }
    let conn = joinset.join_next().await;
    conn.expect("no addrs to connect").expect("failed to join")
}

pub(crate) struct LanMouseConnection {
    server: Server,
    conns: Rc<Mutex<HashMap<SocketAddr, Arc<dyn Conn + Send + Sync>>>>,
    connecting: Rc<Mutex<HashSet<ClientHandle>>>,
}

impl LanMouseConnection {
    pub(crate) fn new(server: Server) -> Self {
        Self {
            server,
            conns: Default::default(),
            connecting: Default::default(),
        }
    }

    pub(crate) async fn send(
        &self,
        event: ProtoEvent,
        handle: ClientHandle,
    ) -> Result<(), LanMouseConnectionError> {
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        let buf = &buf[..len];
        if let Some(addr) = self.server.active_addr(handle) {
            if let Some(conn) = self.conns.lock().await.get(&addr) {
                match conn.send(buf).await {
                    Ok(_) => return Ok(()),
                    Err(e) => {
                        log::warn!("client {handle} failed to connect: {e}");
                        self.conns.lock().await.remove(&addr);
                        self.server.set_active_addr(handle, None);
                    }
                }
            }
        }

        // check if we are already trying to connect
        {
            let mut connecting = self.connecting.lock().await;
            if connecting.contains(&handle) {
                return Err(LanMouseConnectionError::NotConnected);
            } else {
                connecting.insert(handle);
            }
        }
        let server = self.server.clone();
        let conns = self.conns.clone();
        let connecting = self.connecting.clone();

        // connect in the background
        spawn_local(async move {
            // sending did not work, figure out active conn.
            if let Some(addrs) = server.get_ips(handle) {
                let port = server.get_port(handle).unwrap_or(DEFAULT_PORT);
                let addrs = addrs
                    .into_iter()
                    .map(|a| SocketAddr::new(a, port))
                    .collect::<Vec<_>>();
                log::info!("client ({handle}) connecting ... (ips: {addrs:?})");
                let res = connect_any(&addrs).await;
                let (conn, addr) = match res {
                    Ok(c) => c,
                    Err(e) => {
                        connecting.lock().await.remove(&handle);
                        return Err(e);
                    }
                };
                log::info!("client ({handle}) connected @ {addr}");
                server.set_active_addr(handle, Some(addr));
                conns.lock().await.insert(addr, conn);
                connecting.lock().await.remove(&handle);
            }
            Result::<(), LanMouseConnectionError>::Ok(())
        });
        Err(LanMouseConnectionError::NotConnected)
    }
}
