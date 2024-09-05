use std::{io, net::SocketAddr, sync::Arc};
use thiserror::Error;
use tokio::net::UdpSocket;
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
}
