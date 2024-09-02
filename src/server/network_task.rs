use local_channel::mpsc::{Receiver, Sender};
use std::{cell::RefCell, collections::HashMap, io, net::SocketAddr, rc::Rc, sync::Arc};
use webrtc_dtls::{
    config::{ClientAuthType, Config, ExtendedMasterSecretType},
    conn::DTLSConn,
    listener::listen,
};
use webrtc_util::{conn::Listener, Conn};

use thiserror::Error;
use tokio::{
    net::UdpSocket,
    task::{spawn_local, JoinHandle},
};

use crate::crypto;

use super::Server;
use lan_mouse_proto::{ProtoEvent, ProtocolError};

pub(crate) async fn new(
    server: Server,
    udp_recv_tx: Sender<Result<(ProtoEvent, SocketAddr), NetworkError>>,
    udp_send_rx: Receiver<(ProtoEvent, SocketAddr)>,
) -> io::Result<JoinHandle<()>> {
    // bind the udp socket
    let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), server.port.get());

    Ok(spawn_local(async move {
        let sender_rx = Rc::new(RefCell::new(udp_send_rx));
        loop {
            let udp_receiver = spawn_local(listen_dtls(listen_addr, udp_recv_tx.clone()));
            let udp_sender = spawn_local(udp_sender(sender_rx.clone()));
            log::info!("starting sender + receiver");
            tokio::select! {
                e = udp_receiver => panic!("{e:?}"), /* channel closed */
                _ = udp_sender => break, /* channel closed */
                _ = server.cancelled() => break, /* cancellation requested */
            }
        }
    }))
}

async fn listen_dtls(
    listen_addr: SocketAddr,
    udp_recv_tx: Sender<Result<(ProtoEvent, SocketAddr), NetworkError>>,
) -> Result<(), NetworkError> {
    let server_cert = crypto::load_key_and_certificate("alice.pem".into(), "alice.pub.pem".into())?;
    let mut cert_pool = rustls::RootCertStore::empty();
    let certs = crypto::load_certificate("alice.pub.pem".into())?;
    for cert in certs.into_iter() {
        cert_pool.add(cert)?;
    }
    let cfg = Config {
        certificates: vec![server_cert],
        extended_master_secret: ExtendedMasterSecretType::Require,
        client_auth: ClientAuthType::RequireAndVerifyClientCert,
        client_cas: cert_pool,
        ..Default::default()
    };
    let listener = Arc::new(listen(listen_addr, cfg).await?);
    loop {
        while let Ok((conn, addr)) = listener.accept().await {
            let udp_recv_tx = udp_recv_tx.clone();
            spawn_local(async move {
                loop {
                    let mut buf = [0u8; lan_mouse_proto::MAX_EVENT_SIZE];
                    let event: Result<_, NetworkError> = match conn.recv(&mut buf).await {
                        Ok(_len) => match ProtoEvent::try_from(buf) {
                            Ok(e) => Ok((e, addr)),
                            Err(e) => Err(e.into()),
                        },
                        Err(e) => Err(e.into()),
                    };
                    udp_recv_tx.send(event).expect("channel closed");
                }
            });
        }
    }
}

async fn udp_sender(rx: Rc<RefCell<Receiver<(ProtoEvent, SocketAddr)>>>) {
    let mut connection_pool: HashMap<SocketAddr, DTLSConn> = HashMap::new();
    loop {
        log::error!("waiting for event to send ...");
        let (event, addr) = rx.borrow_mut().recv().await.expect("channel closed");
        log::error!("{:20} ------>->->-> {addr}", event.to_string());
        if !connection_pool.contains_key(&addr) {
            let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await.unwrap());
            socket.connect(addr).await.unwrap();
            let certificate =
                crypto::load_key_and_certificate("bob.pem".into(), "bob.pub.pem".into()).unwrap();
            let mut cert_pool = rustls::RootCertStore::empty();
            let certs = crypto::load_certificate("alice.pub.pem".into()).unwrap();
            for cert in certs.into_iter() {
                cert_pool.add(cert.to_owned()).unwrap();
            }
            let config = Config {
                certificates: vec![certificate],
                extended_master_secret: ExtendedMasterSecretType::Require,
                roots_cas: cert_pool,
                server_name: "webrtc.rs".to_owned(),
                ..Default::default()
            };
            log::error!("connecting to {addr}");
            let conn = DTLSConn::new(socket, config, true, None).await.unwrap();
            log::error!("connected {addr}!");
            connection_pool.insert(addr, conn);
        };
        let conn = connection_pool.get(&addr).unwrap();
        log::error!("{:20} ------>->->-> {addr}", event.to_string());
        let (data, len): ([u8; lan_mouse_proto::MAX_EVENT_SIZE], usize) = event.into();
        // When udp blocks, we dont want to block the event loop.
        // Dropping events is better than potentially crashing the input capture.
        conn.send_to(&data[..len], addr).await.unwrap();
    }
}

#[derive(Debug, Error)]
pub(crate) enum NetworkError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("network error: `{0}`")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Crypt(#[from] crypto::Error),
    #[error(transparent)]
    Rustls(#[from] rustls::Error),
    #[error(transparent)]
    WebrtcDtls(#[from] webrtc_dtls::Error),
    #[error(transparent)]
    WebrtcUtil(#[from] webrtc_util::Error),
}
