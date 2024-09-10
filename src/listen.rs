use futures::{Stream, StreamExt};
use lan_mouse_proto::{ProtoEvent, MAX_EVENT_SIZE};
use local_channel::mpsc::{channel, Receiver, Sender};
use std::{net::SocketAddr, rc::Rc, sync::Arc};
use thiserror::Error;
use tokio::{
    sync::Mutex,
    task::{spawn_local, JoinHandle},
};
use webrtc_dtls::{
    config::{Config, ExtendedMasterSecretType},
    crypto::Certificate,
    listener::listen,
};
use webrtc_util::{conn::Listener, Conn, Error};

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
    conns: Rc<Mutex<Vec<Arc<dyn Conn + Send + Sync>>>>,
}

impl LanMouseListener {
    pub(crate) async fn new(port: u16) -> Result<Self, ListenerCreationError> {
        let (listen_tx, listen_rx) = channel();

        let listen_addr = SocketAddr::new("0.0.0.0".parse().expect("invalid ip"), port);
        let certificate = Certificate::generate_self_signed(["localhost".to_owned()])?;
        let cfg = Config {
            certificates: vec![certificate],
            extended_master_secret: ExtendedMasterSecretType::Require,
            ..Default::default()
        };

        let listener = listen(listen_addr, cfg).await?;

        let conns: Rc<Mutex<Vec<Arc<dyn Conn + Send + Sync>>>> = Rc::new(Mutex::new(Vec::new()));

        let conns_clone = conns.clone();

        let tx = listen_tx.clone();
        let listen_task: JoinHandle<()> = spawn_local(async move {
            loop {
                let (conn, addr) = match listener.accept().await {
                    Ok(c) => c,
                    Err(e) => {
                        log::warn!("accept: {e}");
                        continue;
                    }
                };
                log::info!("dtls client connected, ip: {addr}");
                let mut conns = conns_clone.lock().await;
                conns.push(conn.clone());
                spawn_local(read_loop(conns_clone.clone(), conn, tx.clone()));
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
        for conn in conns.iter() {
            let _ = conn.close().await;
        }
        self.listen_tx.close();
    }

    #[allow(unused)]
    pub(crate) async fn broadcast(&self, event: ProtoEvent) {
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        let conns = self.conns.lock().await;
        for conn in conns.iter() {
            let _ = conn.send(&buf[..len]).await;
        }
    }

    pub(crate) async fn reply(&self, addr: SocketAddr, event: ProtoEvent) {
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        let conns = self.conns.lock().await;
        for conn in conns.iter() {
            if conn.remote_addr() == Some(addr) {
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
    conns: Rc<Mutex<Vec<Arc<dyn Conn + Send + Sync>>>>,
    conn: Arc<dyn Conn + Send + Sync>,
    dtls_tx: Sender<(ProtoEvent, SocketAddr)>,
) -> Result<(), Error> {
    let mut b = [0u8; MAX_EVENT_SIZE];

    while let Ok(_) = conn.recv(&mut b).await {
        match b.try_into() {
            Ok(event) => dtls_tx
                .send((event, conn.remote_addr().expect("no remote addr")))
                .expect("channel closed"),
            Err(e) => {
                log::warn!("error receiving event: {e}");
                break;
            }
        }
    }
    log::info!("dtls client disconnected {:?}", conn.remote_addr());
    let mut conns = conns.lock().await;
    let index = conns
        .iter()
        .position(|c| c.remote_addr() == conn.remote_addr())
        .expect("connection not found");
    conns.remove(index);
    Ok(())
}
