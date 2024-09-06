use crate::server::Server;
use lan_mouse_ipc::{ClientHandle, DEFAULT_PORT};
use lan_mouse_proto::{ProtoEvent, MAX_EVENT_SIZE};
use std::{collections::HashMap, io, net::SocketAddr, sync::Arc};
use thiserror::Error;
use tokio::{net::UdpSocket, task::JoinSet};
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
    #[error("no ips associated with the client")]
    NoIps,
}

pub(crate) struct LanMouseConnection {}

impl LanMouseConnection {
    pub(crate) async fn connect(
        addr: SocketAddr,
    ) -> Result<Arc<dyn Conn + Sync + Send>, LanMouseConnectionError> {
        let conn = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
        conn.connect(addr).await;
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
        Ok(dtls_conn)
    }

    pub(crate) async fn connect_any(
        addrs: &[SocketAddr],
    ) -> Result<Arc<dyn Conn + Send + Sync>, LanMouseConnectionError> {
        let mut joinset = JoinSet::new();
        for &addr in addrs {
            joinset.spawn_local(Self::connect(addr));
        }
        let conn = joinset.join_next().await;
        conn.expect("no addrs to connect").expect("failed to join")
    }
}

struct ConnectionProxy {
    server: Server,
    conns: HashMap<SocketAddr, Arc<dyn Conn + Send + Sync>>,
}

impl ConnectionProxy {
    fn find_conn(&self, addrs: &[SocketAddr]) -> Vec<Arc<dyn Conn + Send + Sync>> {
        let mut conns = vec![];
        for addr in addrs {
            if let Some(conn) = self.conns.get(&addr) {
                conns.push(conn.clone());
            }
        }
        conns
    }

    async fn send(
        &self,
        event: ProtoEvent,
        handle: ClientHandle,
    ) -> Result<(), LanMouseConnectionError> {
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        let buf = &buf[..len];
        if let Some(addr) = self.server.active_addr(handle) {
            if let Some(conn) = self.conns.get(&addr) {
                if let Ok(_) = conn.send(buf).await {
                    return Ok(());
                }
            }
        }
        // sending did not work, figure out active conn.
        if let Some(addrs) = self.server.get_ips(handle) {
            let port = self.server.get_port(handle).unwrap_or(DEFAULT_PORT);
            let addrs = addrs
                .into_iter()
                .map(|a| SocketAddr::new(a, port))
                .collect::<Vec<_>>();
            let conn = LanMouseConnection::connect_any(&addrs).await?;
            let addr = conn.remote_addr().expect("no remote addr");
            self.server.set_active_addr(handle, addr);
            conn.send(buf).await?;
            return Ok(());
        }
        Err(LanMouseConnectionError::NoIps)
    }
}
