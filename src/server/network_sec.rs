use std::{io, net::SocketAddr, sync::Arc};

use input_event::{Event, ProtocolError};
use rustls::RootCertStore;
use thiserror::Error;
use tokio::{
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};
use webrtc_dtls::{
    config::{ClientAuthType, Config, ExtendedMasterSecretType},
    listener::listen,
};
use webrtc_util::conn::Listener;

use super::Server;

#[derive(Debug, Error)]
pub(crate) enum NetworkError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("network error: `{0}`")]
    Io(#[from] io::Error),
    #[error(transparent)]
    WebrtcDtls(#[from] webrtc_dtls::Error),
}

pub(crate) async fn new(
    server: Server,
    udp_recv_tx: Sender<Result<(Event, SocketAddr), NetworkError>>,
    udp_send_rx: Receiver<(Event, SocketAddr)>,
) -> Result<JoinHandle<()>, NetworkError> {
    let cfg = Config {
        certificates: vec![],
        client_auth: ClientAuthType::RequireAndVerifyClientCert,
        client_cas: RootCertStore::empty(),
        extended_master_secret: ExtendedMasterSecretType::Require,
        ..Default::default()
    };
    let host = SocketAddr::new("0.0.0.0".parse().unwrap(), server.port.get());
    let listener = Arc::new(listen(host, cfg).await?);
    Ok(tokio::task::spawn_local(async move {
        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((dtls_conn, addr))  => {
                            let udp_recv_tx = udp_recv_tx.clone();
                            tokio::task::spawn_local(async move {
                                loop {
                                    let mut buf = vec![0u8; 0];
                                    while let Ok(_) = dtls_conn.recv(&mut buf).await {
                                        let event = buf.as_slice().try_into()?;
                                        let addr = dtls_conn.remote_addr()?;
                                        let event = Ok((event, addr));
                                        udp_recv_tx.send(event).await;
                                    }
                                }
                                Ok::<(),NetworkError>(())
                            });
                        },
                        Err(e) => log::warn!("connecting failed {e}"),
                    }
                }
                _ = server.cancelled() => break,
            }
        }
    }))
}
