use futures::{Stream, StreamExt};
use lan_mouse_proto::{ProtoEvent, MAX_EVENT_SIZE};
use local_channel::mpsc::{channel, Receiver, Sender};
use rustls::pki_types::CertificateDer;
use std::{
    collections::HashMap,
    net::SocketAddr,
    rc::Rc,
    sync::{Arc, RwLock},
    time::Duration,
};
use thiserror::Error;
use tokio::{
    sync::Mutex,
    task::{spawn_local, JoinHandle},
};
use webrtc_dtls::{
    config::{ClientAuthType::RequireAnyClientCert, Config, ExtendedMasterSecretType},
    conn::DTLSConn,
    crypto::Certificate,
    listener::listen,
};
use webrtc_util::{conn::Listener, Conn, Error};

use crate::crypto;

#[derive(Error, Debug)]
pub enum ListenerCreationError {
    #[error(transparent)]
    WebrtcUtil(#[from] webrtc_util::Error),
    #[error(transparent)]
    WebrtcDtls(#[from] webrtc_dtls::Error),
}

type ArcConn = Arc<dyn Conn + Send + Sync>;

pub(crate) struct LanMouseListener {
    listen_rx: Receiver<(ProtoEvent, SocketAddr)>,
    listen_tx: Sender<(ProtoEvent, SocketAddr)>,
    listen_task: JoinHandle<()>,
    conns: Rc<Mutex<Vec<(SocketAddr, ArcConn)>>>,
    request_port_change: Sender<u16>,
    port_changed: Receiver<Result<u16, ListenerCreationError>>,
}

type VerifyPeerCertificateFn = Arc<
    dyn (Fn(&[Vec<u8>], &[CertificateDer<'static>]) -> Result<(), webrtc_dtls::Error>)
        + Send
        + Sync,
>;

impl LanMouseListener {
    pub(crate) async fn new(
        port: u16,
        cert: Certificate,
        authorized_keys: Arc<RwLock<HashMap<String, String>>>,
    ) -> Result<Self, ListenerCreationError> {
        let (listen_tx, listen_rx) = channel();
        let (request_port_change, mut request_port_change_rx) = channel();
        let (port_changed_tx, port_changed) = channel();

        let authorized = authorized_keys.clone();
        let verify_peer_certificate: Option<VerifyPeerCertificateFn> = Some(Arc::new(
            move |certs: &[Vec<u8>], _chains: &[CertificateDer<'static>]| {
                assert!(certs.len() == 1);
                let fingerprints = certs
                    .iter()
                    .map(|c| crypto::generate_fingerprint(c))
                    .collect::<Vec<_>>();
                if authorized
                    .read()
                    .expect("lock")
                    .contains_key(&fingerprints[0])
                {
                    Ok(())
                } else {
                    Err(webrtc_dtls::Error::ErrVerifyDataMismatch)
                }
            },
        ));
        let cfg = Config {
            certificates: vec![cert.clone()],
            extended_master_secret: ExtendedMasterSecretType::Require,
            client_auth: RequireAnyClientCert,
            verify_peer_certificate,
            ..Default::default()
        };

        let listen_addr = SocketAddr::new("0.0.0.0".parse().expect("invalid ip"), port);
        let mut listener = listen(listen_addr, cfg.clone()).await?;

        let conns: Rc<Mutex<Vec<(SocketAddr, ArcConn)>>> = Rc::new(Mutex::new(Vec::new()));

        let conns_clone = conns.clone();
        let tx = listen_tx.clone();
        let listen_task: JoinHandle<()> = spawn_local(async move {
            loop {
                let sleep = tokio::time::sleep(Duration::from_secs(2));
                tokio::select! {
                    /* workaround for https://github.com/webrtc-rs/webrtc/issues/614 */
                    _ = sleep => continue,
                    c = listener.accept() => match c {
                        Ok((conn, addr)) => {
                            log::info!("dtls client connected, ip: {addr}");
                            let mut conns = conns_clone.lock().await;
                            conns.push((addr, conn.clone()));
                            spawn_local(read_loop(conns_clone.clone(), addr, conn, tx.clone()));
                        },
                        Err(e) => log::warn!("accept: {e}"),
                    },
                    port = request_port_change_rx.recv() => {
                        let port = port.expect("channel closed");
                        let listen_addr = SocketAddr::new("0.0.0.0".parse().expect("invalid ip"), port);
                        match listen(listen_addr, cfg.clone()).await {
                            Ok(new_listener) => {
                                let _ = listener.close().await;
                                listener = new_listener;
                                port_changed_tx.send(Ok(port)).expect("channel closed");
                            }
                            Err(e) => {
                                log::warn!("unable to change port: {e}");
                                port_changed_tx.send(Err(e.into())).expect("channel closed");
                            }
                        };
                    },
                };
            }
        });

        Ok(Self {
            conns,
            listen_rx,
            listen_tx,
            listen_task,
            port_changed,
            request_port_change,
        })
    }

    pub(crate) fn request_port_change(&mut self, port: u16) {
        self.request_port_change.send(port).expect("channel closed");
    }

    pub(crate) async fn port_changed(&mut self) -> Result<u16, ListenerCreationError> {
        self.port_changed.recv().await.expect("channel closed")
    }

    pub(crate) async fn terminate(&mut self) {
        self.listen_task.abort();
        let conns = self.conns.lock().await;
        for (_, conn) in conns.iter() {
            let _ = conn.close().await;
        }
        self.listen_tx.close();
    }

    pub(crate) async fn reply(&self, addr: SocketAddr, event: ProtoEvent) {
        log::trace!("reply {event} >=>=>=>=>=> {addr}");
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        let conns = self.conns.lock().await;
        for (a, conn) in conns.iter() {
            if *a == addr {
                let _ = conn.send(&buf[..len]).await;
            }
        }
    }

    pub(crate) async fn get_certificate_fingerprint(&self, addr: SocketAddr) -> Option<String> {
        if let Some(conn) = self
            .conns
            .lock()
            .await
            .iter()
            .find(|(a, _)| *a == addr)
            .map(|(_, c)| c.clone())
        {
            let conn: &DTLSConn = conn.as_any().downcast_ref().expect("dtls conn");
            let certs = conn.connection_state().await.peer_certificates;
            let cert = certs.first()?;
            let fingerprint = crypto::generate_fingerprint(cert);
            Some(fingerprint)
        } else {
            None
        }
    }
}

impl Stream for LanMouseListener {
    type Item = (ProtoEvent, SocketAddr);

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.listen_rx.poll_next_unpin(cx)
    }
}

async fn read_loop(
    conns: Rc<Mutex<Vec<(SocketAddr, ArcConn)>>>,
    addr: SocketAddr,
    conn: ArcConn,
    dtls_tx: Sender<(ProtoEvent, SocketAddr)>,
) -> Result<(), Error> {
    let mut b = [0u8; MAX_EVENT_SIZE];

    while conn.recv(&mut b).await.is_ok() {
        match b.try_into() {
            Ok(event) => dtls_tx.send((event, addr)).expect("channel closed"),
            Err(e) => {
                log::warn!("error receiving event: {e}");
                break;
            }
        }
    }
    log::info!("dtls client disconnected {:?}", addr);
    let mut conns = conns.lock().await;
    let index = conns
        .iter()
        .position(|(a, _)| *a == addr)
        .expect("connection not found");
    conns.remove(index);
    Ok(())
}
