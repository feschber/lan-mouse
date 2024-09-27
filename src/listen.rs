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

pub(crate) struct LanMouseListener {
    listen_rx: Receiver<(ProtoEvent, SocketAddr)>,
    listen_tx: Sender<(ProtoEvent, SocketAddr)>,
    listen_task: JoinHandle<()>,
    conns: Rc<Mutex<Vec<(SocketAddr, Arc<dyn Conn + Send + Sync>)>>>,
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

        let listen_addr = SocketAddr::new("0.0.0.0".parse().expect("invalid ip"), port);
        let verify_peer_certificate: Option<VerifyPeerCertificateFn> = Some(Arc::new(
            move |certs: &[Vec<u8>], _chains: &[CertificateDer<'static>]| {
                assert!(certs.len() == 1);
                let fingerprints = certs
                    .into_iter()
                    .map(|c| crypto::generate_fingerprint(c))
                    .collect::<Vec<_>>();
                if authorized_keys
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
            certificates: vec![cert],
            extended_master_secret: ExtendedMasterSecretType::Require,
            client_auth: RequireAnyClientCert,
            verify_peer_certificate,
            ..Default::default()
        };

        let listener = listen(listen_addr, cfg).await?;

        let conns: Rc<Mutex<Vec<(SocketAddr, Arc<dyn Conn + Send + Sync>)>>> =
            Rc::new(Mutex::new(Vec::new()));

        let conns_clone = conns.clone();

        let tx = listen_tx.clone();
        let listen_task: JoinHandle<()> = spawn_local(async move {
            loop {
                let sleep = tokio::time::sleep(Duration::from_secs(2));
                let (conn, addr) = tokio::select! {
                    _ = sleep => continue,
                    c = listener.accept() => match c {
                        Ok(c) => c,
                        Err(e) => {
                            log::warn!("accept: {e}");
                            continue;
                        }
                    },
                };
                log::info!("dtls client connected, ip: {addr}");
                let mut conns = conns_clone.lock().await;
                conns.push((addr, conn.clone()));
                spawn_local(read_loop(conns_clone.clone(), addr, conn, tx.clone()));
            }
        });

        Ok(Self {
            conns,
            listen_rx,
            listen_tx,
            listen_task,
        })
    }

    pub(crate) async fn terminate(&mut self) {
        self.listen_task.abort();
        let conns = self.conns.lock().await;
        for (_, conn) in conns.iter() {
            let _ = conn.close().await;
        }
        self.listen_tx.close();
    }

    #[allow(unused)]
    pub(crate) async fn broadcast(&self, event: ProtoEvent) {
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        let conns = self.conns.lock().await;
        for (_, conn) in conns.iter() {
            let _ = conn.send(&buf[..len]).await;
        }
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
    conns: Rc<Mutex<Vec<(SocketAddr, Arc<dyn Conn + Send + Sync>)>>>,
    addr: SocketAddr,
    conn: Arc<dyn Conn + Send + Sync>,
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
