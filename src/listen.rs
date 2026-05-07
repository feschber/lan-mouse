use futures::{Stream, StreamExt};
use lan_mouse_ipc::IncomingPeerConfig;
use lan_mouse_proto::{MAX_EVENT_SIZE, ProtoEvent};
use local_channel::mpsc::{Receiver, Sender, channel};
use rustls::pki_types::CertificateDer;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::{IpAddr, SocketAddr},
    rc::Rc,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use thiserror::Error;
use tokio::time::MissedTickBehavior;
use tokio::{
    sync::Mutex as AsyncMutex,
    task::{JoinHandle, spawn_local},
};
use webrtc_dtls::{
    config::{ClientAuthType::RequireAnyClientCert, Config, ExtendedMasterSecretType},
    conn::DTLSConn,
    crypto::Certificate,
    listener::listen,
};
use webrtc_util::{Conn, Error, conn::Listener};

use crate::crypto;

#[derive(Error, Debug)]
pub enum ListenerCreationError {
    #[error(transparent)]
    WebrtcUtil(#[from] webrtc_util::Error),
    #[error(transparent)]
    WebrtcDtls(#[from] webrtc_dtls::Error),
    #[error("no listener could be bound on any local address")]
    NoBoundListener,
}

type ArcConn = Arc<dyn Conn + Send + Sync>;
type DynListener = Box<dyn Listener + Send + Sync>;

pub(crate) enum ListenEvent {
    Msg {
        event: ProtoEvent,
        addr: SocketAddr,
    },
    Accept {
        addr: SocketAddr,
        fingerprint: String,
    },
    Rejected {
        fingerprint: String,
    },
}

pub(crate) struct LanMouseListener {
    listen_rx: Receiver<ListenEvent>,
    listen_tx: Sender<ListenEvent>,
    listen_task: JoinHandle<()>,
    conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>>,
    request_port_change: Sender<u16>,
    port_changed: Receiver<Result<u16, ListenerCreationError>>,
}

type VerifyPeerCertificateFn = Arc<
    dyn (Fn(&[Vec<u8>], &[CertificateDer<'static>]) -> Result<(), webrtc_dtls::Error>)
        + Send
        + Sync,
>;

/// One bound DTLS listener and the task that accepts on it. Stored
/// in a `HashMap<IpAddr, ListenerSlot>` keyed by the local IPv4
/// address it's bound to so the supervisor can plug/unplug
/// listeners as interfaces appear/disappear.
struct ListenerSlot {
    /// Background task that calls `listener.accept()` in a loop and
    /// forwards events into the shared `listen_tx` / `conns`.
    /// Aborted on `Drop` so dropping the supervisor cleans up.
    accept_task: JoinHandle<()>,
}

impl Drop for ListenerSlot {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

impl LanMouseListener {
    pub(crate) async fn new(
        port: u16,
        cert: Certificate,
        authorized_keys: Arc<RwLock<HashMap<String, IncomingPeerConfig>>>,
    ) -> Result<Self, ListenerCreationError> {
        let (listen_tx, listen_rx) = channel();
        let (request_port_change, request_port_change_rx) = channel();
        let (port_changed_tx, port_changed) = channel();
        let connection_attempts: Arc<Mutex<VecDeque<String>>> = Default::default();

        let authorized = authorized_keys.clone();
        let verify_peer_certificate: Option<VerifyPeerCertificateFn> = {
            let connection_attempts = connection_attempts.clone();
            Some(Arc::new(
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
                        let fingerprint = fingerprints.into_iter().next().expect("fingerprint");
                        connection_attempts
                            .lock()
                            .expect("lock")
                            .push_back(fingerprint);
                        Err(webrtc_dtls::Error::ErrVerifyDataMismatch)
                    }
                },
            ))
        };
        let cfg = Config {
            certificates: vec![cert.clone()],
            extended_master_secret: ExtendedMasterSecretType::Require,
            client_auth: RequireAnyClientCert,
            verify_peer_certificate,
            ..Default::default()
        };

        let conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>> =
            Rc::new(AsyncMutex::new(Vec::new()));

        // Bind one listener per local IPv4 address (skip loopback +
        // link-local) instead of a single 0.0.0.0:port listener. With
        // a single 0.0.0.0 bind on a multi-homed host, replies use
        // the kernel's preferred outbound interface as source IP —
        // which may not match the IP the peer dialed, breaking QUIC
        // 4-tuple matching. Per-IP binds make replies symmetric
        // automatically: each listener's reply socket is bound to a
        // specific IP, so the kernel uses *that* IP as source.
        let initial_addrs = enumerate_listenable_ipv4();
        if initial_addrs.is_empty() {
            // Fall back to 0.0.0.0 so we at least listen somewhere if
            // interface enumeration fails (very unusual).
            log::warn!("no listenable IPv4 addresses found; falling back to 0.0.0.0");
        }
        let mut listeners: HashMap<IpAddr, ListenerSlot> = HashMap::new();
        let mut bound_count = 0usize;
        for ip in &initial_addrs {
            match try_bind_listener(*ip, port, &cfg).await {
                Ok(listener) => {
                    let task = spawn_accept_task(
                        listener,
                        listen_tx.clone(),
                        conns.clone(),
                        connection_attempts.clone(),
                    );
                    listeners.insert(*ip, ListenerSlot { accept_task: task });
                    bound_count += 1;
                    log::info!("listening for DTLS on {ip}:{port}");
                }
                Err(e) => log::warn!("failed to bind listener on {ip}:{port}: {e}"),
            }
        }
        if bound_count == 0 {
            // Either enumeration returned no addrs, or every bind
            // failed. Try `0.0.0.0:port` as a last resort.
            let fallback = IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);
            match try_bind_listener(fallback, port, &cfg).await {
                Ok(listener) => {
                    let task = spawn_accept_task(
                        listener,
                        listen_tx.clone(),
                        conns.clone(),
                        connection_attempts.clone(),
                    );
                    listeners.insert(fallback, ListenerSlot { accept_task: task });
                    log::info!(
                        "listening for DTLS on {fallback}:{port} (fallback — symmetric replies not guaranteed)"
                    );
                }
                Err(e) => return Err(e),
            }
        }

        let listen_task = spawn_supervisor_task(
            port,
            cfg,
            listeners,
            listen_tx.clone(),
            conns.clone(),
            connection_attempts,
            request_port_change_rx,
            port_changed_tx,
        );

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
    type Item = ListenEvent;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.listen_rx.poll_next_unpin(cx)
    }
}

/// Enumerate local IPv4 addresses suitable for binding a public
/// listener: skip loopback (127.0.0.0/8) and link-local
/// (169.254.0.0/16) since neither is reachable from a peer. IPv6
/// is intentionally omitted — lan-mouse is IPv4-only on the wire
/// today.
fn enumerate_listenable_ipv4() -> Vec<IpAddr> {
    let ifaces = match if_addrs::get_if_addrs() {
        Ok(v) => v,
        Err(e) => {
            log::warn!("get_if_addrs failed: {e}");
            return Vec::new();
        }
    };
    ifaces
        .into_iter()
        .filter_map(|iface| match iface.addr {
            if_addrs::IfAddr::V4(v4) => Some(v4.ip),
            if_addrs::IfAddr::V6(_) => None,
        })
        .filter(|ip| !ip.is_loopback() && !ip.is_link_local())
        .map(IpAddr::V4)
        .collect()
}

async fn try_bind_listener(
    ip: IpAddr,
    port: u16,
    cfg: &Config,
) -> Result<DynListener, ListenerCreationError> {
    let addr = SocketAddr::new(ip, port);
    let listener = listen(addr, cfg.clone()).await?;
    Ok(Box::new(listener))
}

/// Spawn an accept loop for one bound listener. Each accepted
/// `(conn, addr)` is registered in the shared `conns` vec and an
/// `Accept` event is published. Verify-peer-certificate failures are
/// re-published as `Rejected` so the UI can surface unauthorized
/// fingerprints. The accept loop exits when the listener errors
/// out or its task is aborted (interface went down, port changed).
fn spawn_accept_task(
    listener: DynListener,
    listen_tx: Sender<ListenEvent>,
    conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>>,
    connection_attempts: Arc<Mutex<VecDeque<String>>>,
) -> JoinHandle<()> {
    spawn_local(async move {
        loop {
            // workaround for https://github.com/webrtc-rs/webrtc/issues/614
            let sleep = tokio::time::sleep(Duration::from_secs(2));
            tokio::select! {
                _ = sleep => continue,
                c = listener.accept() => match c {
                    Ok((conn, addr)) => {
                        log::info!("dtls client connected, ip: {addr}");
                        {
                            let mut conns_guard = conns.lock().await;
                            conns_guard.push((addr, conn.clone()));
                        }
                        let dtls_conn: &DTLSConn = conn.as_any().downcast_ref().expect("dtls conn");
                        let certs = dtls_conn.connection_state().await.peer_certificates;
                        let cert = certs.first().expect("cert");
                        let fingerprint = crypto::generate_fingerprint(cert);
                        listen_tx
                            .send(ListenEvent::Accept { addr, fingerprint })
                            .expect("channel closed");
                        spawn_local(read_loop(conns.clone(), addr, conn, listen_tx.clone()));
                    }
                    Err(e) => {
                        if let Error::Std(ref se) = e {
                            if let Some(de) = se.0.downcast_ref::<webrtc_dtls::Error>() {
                                match de {
                                    webrtc_dtls::Error::ErrVerifyDataMismatch => {
                                        if let Some(fingerprint) =
                                            connection_attempts.lock().expect("lock").pop_front()
                                        {
                                            listen_tx
                                                .send(ListenEvent::Rejected { fingerprint })
                                                .expect("channel closed");
                                        }
                                    }
                                    _ => log::warn!("accept: {de}"),
                                }
                            } else {
                                log::warn!("accept: {se:?}");
                            }
                        } else {
                            log::warn!("accept: {e:?}");
                        }
                    }
                },
            };
        }
    })
}

/// Supervisor task: owns the set of active listeners, watches for
/// interface up/down events via `if_watch`, and rebuilds listeners
/// on port change. Each listener slot is keyed by its local IPv4
/// address; on `IfEvent::Up` we add a slot, on `IfEvent::Down` we
/// drop one (which aborts its accept task).
#[allow(clippy::too_many_arguments)]
fn spawn_supervisor_task(
    initial_port: u16,
    cfg: Config,
    initial_listeners: HashMap<IpAddr, ListenerSlot>,
    listen_tx: Sender<ListenEvent>,
    conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>>,
    connection_attempts: Arc<Mutex<VecDeque<String>>>,
    mut request_port_change_rx: Receiver<u16>,
    port_changed_tx: Sender<Result<u16, ListenerCreationError>>,
) -> JoinHandle<()> {
    spawn_local(async move {
        let mut port = initial_port;
        let mut listeners = initial_listeners;
        let mut watcher = match if_watch::tokio::IfWatcher::new() {
            Ok(w) => Some(w),
            Err(e) => {
                log::warn!(
                    "if_watch::IfWatcher::new failed: {e}; interface plug/unplug \
                     will not be detected (restart lan-mouse to pick up new addrs)"
                );
                None
            }
        };
        // Periodic reconciliation: enumerate live IPs and diff against
        // `listeners`. Network.framework on macOS doesn't reliably fire
        // `IfEvent::Down` when an interface is administratively
        // disabled (e.g. user toggles Wi-Fi off in System Settings),
        // leaving stale slots bound to vanished IPs that no traffic
        // can reach. Polling every 30s catches whatever if-watch
        // misses — both adds (covers missed Up events too) and drops.
        // `Skip` so a long suspend (laptop closed for hours) doesn't
        // burst-fire backlog ticks at resume.
        let mut reconcile_tick = tokio::time::interval(Duration::from_secs(30));
        reconcile_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        // Skip the immediate-first tick — we just enumerated at startup
        // and don't want to thrash listeners on the first iteration.
        reconcile_tick.tick().await;
        loop {
            tokio::select! {
                _ = reconcile_tick.tick() => {
                    let current_ips: HashSet<IpAddr> =
                        enumerate_listenable_ipv4().into_iter().collect();
                    let to_drop: Vec<IpAddr> = listeners
                        .keys()
                        .filter(|ip| !current_ips.contains(*ip))
                        .copied()
                        .collect();
                    for ip in to_drop {
                        // `to_drop` was just collected from
                        // `listeners.keys()` and we run single-
                        // threaded, so the remove always returns Some.
                        listeners.remove(&ip);
                        log::info!(
                            "reconcile: dropping stale listener on {ip}:{port} \
                             (IP no longer present on any interface)"
                        );
                    }
                    // `try_bind_listener` is async and may fail, so
                    // `entry().or_insert_with(...)` doesn't fit — we
                    // only want to insert on bind success. Match the
                    // `Entry::Vacant` slot up front so the same hash
                    // lookup covers both the existence check and the
                    // later insert, satisfying clippy::map_entry.
                    for ip in current_ips {
                        if let std::collections::hash_map::Entry::Vacant(slot) =
                            listeners.entry(ip)
                        {
                            match try_bind_listener(ip, port, &cfg).await {
                                Ok(l) => {
                                    let task = spawn_accept_task(
                                        l,
                                        listen_tx.clone(),
                                        conns.clone(),
                                        connection_attempts.clone(),
                                    );
                                    slot.insert(ListenerSlot { accept_task: task });
                                    log::info!(
                                        "reconcile: now listening on {ip}:{port} \
                                         (IP appeared without an Up event)"
                                    );
                                }
                                Err(e) => log::warn!(
                                    "reconcile: failed to bind on {ip}:{port}: {e}"
                                ),
                            }
                        }
                    }
                }
                ev = async {
                    match watcher.as_mut() {
                        Some(w) => w.select_next_some().await,
                        None => std::future::pending().await,
                    }
                } => match ev {
                    Ok(if_watch::IfEvent::Up(net)) => {
                        let ip = net.addr();
                        let usable = if let IpAddr::V4(v4) = ip {
                            !v4.is_loopback() && !v4.is_link_local()
                        } else {
                            false
                        };
                        if usable && !listeners.contains_key(&ip) {
                            match try_bind_listener(ip, port, &cfg).await {
                                Ok(l) => {
                                    let task = spawn_accept_task(
                                        l,
                                        listen_tx.clone(),
                                        conns.clone(),
                                        connection_attempts.clone(),
                                    );
                                    listeners.insert(ip, ListenerSlot { accept_task: task });
                                    log::info!("interface up: now listening on {ip}:{port}");
                                }
                                Err(e) => log::warn!("failed to bind on {ip}:{port}: {e}"),
                            }
                        }
                    }
                    Ok(if_watch::IfEvent::Down(net)) => {
                        let ip = net.addr();
                        if listeners.remove(&ip).is_some() {
                            log::info!("interface down: stopped listening on {ip}:{port}");
                        }
                    }
                    Err(e) => log::debug!("if_watch error: {e}"),
                },
                p = request_port_change_rx.recv() => {
                    let new_port = p.expect("channel closed");
                    listeners.clear(); // Drop aborts each accept task
                    let mut bound = 0usize;
                    let addrs = enumerate_listenable_ipv4();
                    for ip in &addrs {
                        match try_bind_listener(*ip, new_port, &cfg).await {
                            Ok(l) => {
                                let task = spawn_accept_task(
                                    l,
                                    listen_tx.clone(),
                                    conns.clone(),
                                    connection_attempts.clone(),
                                );
                                listeners.insert(*ip, ListenerSlot { accept_task: task });
                                bound += 1;
                            }
                            Err(e) => log::warn!("port change: failed to bind {ip}:{new_port}: {e}"),
                        }
                    }
                    if bound == 0 {
                        port_changed_tx
                            .send(Err(ListenerCreationError::NoBoundListener))
                            .expect("channel closed");
                    } else {
                        port = new_port;
                        port_changed_tx
                            .send(Ok(port))
                            .expect("channel closed");
                    }
                }
            }
        }
    })
}

async fn read_loop(
    conns: Rc<AsyncMutex<Vec<(SocketAddr, ArcConn)>>>,
    addr: SocketAddr,
    conn: ArcConn,
    dtls_tx: Sender<ListenEvent>,
) -> Result<(), Error> {
    let mut b = [0u8; MAX_EVENT_SIZE];

    while conn.recv(&mut b).await.is_ok() {
        match b.try_into() {
            Ok(event) => dtls_tx
                .send(ListenEvent::Msg { event, addr })
                .expect("channel closed"),
            Err(e) => {
                // Skip the malformed/unknown datagram and keep
                // listening. Each DTLS recv returns one full
                // datagram, so a parse error here can't desync a
                // stream; the next call gets a fresh, framed
                // message. This makes the protocol forward-
                // compatible: a peer running a newer Lan Mouse
                // version can introduce additional event types
                // and old peers will simply ignore them rather
                // than dropping the connection.
                log::debug!("ignoring undecodable event from {addr}: {e}");
            }
        }
    }
    log::info!("dtls client disconnected {addr:?}");
    let mut conns = conns.lock().await;
    let index = conns
        .iter()
        .position(|(a, _)| *a == addr)
        .expect("connection not found");
    conns.remove(index);
    Ok(())
}
