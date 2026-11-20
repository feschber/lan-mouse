use crate::{client::ClientManager, config::local_commit, dns::DnsLookup};
use lan_mouse_ipc::{ClientHandle, DEFAULT_PORT};
use lan_mouse_proto::{MAX_EVENT_SIZE, ProtoEvent};
use local_channel::mpsc::{Receiver, Sender, channel};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io,
    net::SocketAddr,
    rc::Rc,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::{
    net::UdpSocket,
    sync::watch,
    task::{JoinHandle, JoinSet, spawn_local},
    time::Instant,
};
use tokio_util::sync::CancellationToken;
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
    #[error("emulation is disabled on the target device")]
    TargetEmulationDisabled,
    #[error("Connection timed out")]
    Timeout,
}

const DEFAULT_CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(500);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(2);

type Transport = Arc<dyn Conn + Send + Sync>;

async fn connect(
    addr: SocketAddr,
    cert: Certificate,
) -> Result<(Transport, SocketAddr), (SocketAddr, LanMouseConnectionError)> {
    log::info!("connecting to {addr} ...");
    let bind_addr = if addr.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    };
    let conn = Arc::new(
        UdpSocket::bind(bind_addr)
            .await
            .map_err(|e| (addr, e.into()))?,
    );
    conn.connect(addr).await.map_err(|e| (addr, e.into()))?;
    let config = Config {
        certificates: vec![cert],
        server_name: "ignored".to_owned(),
        insecure_skip_verify: true,
        extended_master_secret: ExtendedMasterSecretType::Require,
        ..Default::default()
    };
    tokio::select! {
        _ = tokio::time::sleep(DEFAULT_CONNECTION_TIMEOUT) => {
            Err((addr, LanMouseConnectionError::Timeout))
        },
        result = DTLSConn::new(conn, config, true, None) => match result {
            Ok(dtls_conn) => Ok((Arc::new(dtls_conn), addr)),
            Err(error) => Err((addr, error.into())),
        }
    }
}

async fn connect_any(
    addrs: &[SocketAddr],
    cert: Certificate,
) -> Result<(Transport, SocketAddr), LanMouseConnectionError> {
    let mut attempts = JoinSet::new();
    for &addr in addrs {
        attempts.spawn_local(connect(addr, cert.clone()));
    }
    while let Some(result) = attempts.join_next().await {
        match result {
            Ok(Ok(connection)) => return Ok(connection),
            Ok(Err((addr, error))) => log::warn!("failed to connect to {addr}: {error}"),
            Err(error) => log::warn!("connection task failed: {error}"),
        }
    }
    Err(LanMouseConnectionError::NotConnected)
}

struct ActiveConnection {
    conn: Transport,
    failed: CancellationToken,
    settings: watch::Receiver<bool>,
}

struct ConnectionContext {
    cert: Certificate,
    clients: ClientManager,
    dns: DnsLookup,
    conns: RefCell<HashMap<ClientHandle, ActiveConnection>>,
    tx: Sender<(ClientHandle, ProtoEvent)>,
    stop: CancellationToken,
}

struct ConnectionWorker {
    settings: watch::Receiver<bool>,
    task: JoinHandle<()>,
}

pub(crate) struct LanMouseConnection {
    context: Rc<ConnectionContext>,
    workers: RefCell<HashMap<ClientHandle, ConnectionWorker>>,
    recv_rx: Receiver<(ClientHandle, ProtoEvent)>,
}

impl LanMouseConnection {
    pub(crate) fn new(cert: Certificate, clients: ClientManager, dns: DnsLookup) -> Self {
        let (tx, recv_rx) = channel();
        Self {
            context: Rc::new(ConnectionContext {
                cert,
                clients,
                dns,
                tx,
                conns: Default::default(),
                stop: CancellationToken::new(),
            }),
            workers: Default::default(),
            recv_rx,
        }
    }

    pub(crate) async fn recv(&mut self) -> (ClientHandle, ProtoEvent) {
        self.recv_rx.recv().await.expect("channel closed")
    }

    pub(crate) async fn send(
        &self,
        event: ProtoEvent,
        handle: ClientHandle,
    ) -> Result<(), LanMouseConnectionError> {
        let active = self.context.conns.borrow().get(&handle).and_then(|active| {
            if active.failed.is_cancelled() || !matches!(active.settings.has_changed(), Ok(false)) {
                None
            } else {
                Some((active.conn.clone(), active.failed.clone()))
            }
        });
        if let Some((conn, failed)) = active {
            if !self.context.clients.alive(handle) {
                return Err(LanMouseConnectionError::TargetEmulationDisabled);
            }
            let (buf, len) = event.into();
            if let Err(error) = conn.send(&buf[..len]).await {
                failed.cancel();
                return Err(error.into());
            }
            return Ok(());
        }
        self.ensure_worker(handle);
        Err(LanMouseConnectionError::NotConnected)
    }

    fn ensure_worker(&self, handle: ClientHandle) {
        let Some(settings) = self.context.clients.connection_changes(handle) else {
            return;
        };
        if !*settings.borrow() || self.context.stop.is_cancelled() {
            return;
        }
        let mut workers = self.workers.borrow_mut();
        if workers.get(&handle).is_some_and(|worker| {
            worker.settings.same_channel(&settings) && !worker.task.is_finished()
        }) {
            return;
        }
        let task = spawn_local(run_connection(
            self.context.clone(),
            handle,
            settings.clone(),
        ));
        workers.insert(handle, ConnectionWorker { settings, task });
    }

    pub(crate) async fn terminate(&mut self) {
        self.context.stop.cancel();
        let workers = std::mem::take(self.workers.get_mut());
        for (_, worker) in workers {
            if let Err(error) = worker.task.await {
                log::warn!("connection worker failed: {error}");
            }
        }
    }
}

impl Drop for LanMouseConnection {
    fn drop(&mut self) {
        self.context.stop.cancel();
    }
}

impl ConnectionContext {
    async fn connect_to_handle(
        &self,
        handle: ClientHandle,
        settings: watch::Receiver<bool>,
    ) -> Result<(Transport, SocketAddr), LanMouseConnectionError> {
        if let Some(hostname) = self.clients.get_hostname(handle) {
            // The resolver acknowledges only after the service applies fresh IPs.
            if !self.dns.resolve(handle, hostname, settings).await {
                return Err(LanMouseConnectionError::NotConnected);
            }
        }
        let ips = self
            .clients
            .get_ips(handle)
            .ok_or(LanMouseConnectionError::NotConnected)?;
        let port = self.clients.get_port(handle).unwrap_or(DEFAULT_PORT);
        let addrs = ips
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect::<Vec<_>>();
        log::info!("client {handle} connecting using refreshed addresses: {addrs:?}");
        connect_any(&addrs, self.cert.clone()).await
    }

    async fn disconnect(
        &self,
        handle: ClientHandle,
        conn: &Transport,
        settings: &watch::Receiver<bool>,
    ) {
        let owns_connection = self
            .conns
            .borrow()
            .get(&handle)
            .is_some_and(|active| Arc::ptr_eq(&active.conn, conn));
        if owns_connection {
            self.conns.borrow_mut().remove(&handle);
            // A removed slab slot may already belong to a different client.
            if self
                .clients
                .connection_changes(handle)
                .is_some_and(|current| current.same_channel(settings))
            {
                self.clients.set_active_addr(handle, None);
                self.clients.set_alive(handle, false);
                self.clients.set_peer_commit(handle, None);
            }
        }
        // Cleanup must not block reconnection when a dead DTLS peer cannot close.
        let _ = tokio::time::timeout(INITIAL_RETRY_DELAY, conn.close()).await;
    }
}

async fn run_connection(
    context: Rc<ConnectionContext>,
    handle: ClientHandle,
    mut settings: watch::Receiver<bool>,
) {
    let mut retry_delay = INITIAL_RETRY_DELAY;
    loop {
        if !*settings.borrow_and_update() || context.stop.is_cancelled() {
            return;
        }
        let attempt = context.connect_to_handle(handle, settings.clone());
        let connection = tokio::select! {
            biased;
            _ = context.stop.cancelled() => return,
            changed = settings.changed() => {
                if changed.is_err() { return; }
                retry_delay = INITIAL_RETRY_DELAY;
                continue;
            },
            result = attempt => result,
        };
        if let Ok((conn, addr)) = connection {
            let failed = CancellationToken::new();
            context.clients.set_alive(handle, false);
            context.clients.set_active_addr(handle, Some(addr));
            context.conns.borrow_mut().insert(
                handle,
                ActiveConnection {
                    conn: conn.clone(),
                    failed: failed.clone(),
                    settings: settings.clone(),
                },
            );
            log::info!("client {handle} connected @ {addr}");
            let last_pong = Cell::new(None);
            let changed = tokio::select! {
                biased;
                _ = context.stop.cancelled() => true,
                _ = settings.changed() => true,
                _ = failed.cancelled() => false,
                _ = receive_loop(&context, handle, &conn, &last_pong) => false,
                _ = ping_pong(&conn, &last_pong) => false,
            };
            context.disconnect(handle, &conn, &settings).await;
            if changed {
                retry_delay = INITIAL_RETRY_DELAY;
                if settings.has_changed().is_err() {
                    return;
                }
                continue;
            }
            if last_pong.get().is_some() {
                retry_delay = INITIAL_RETRY_DELAY;
            }
        } else if let Err(error) = connection {
            log::warn!("client {handle} connection failed: {error}");
        }
        log::info!("client {handle} retrying resolution and connection in {retry_delay:?}");
        tokio::select! {
            biased;
            _ = context.stop.cancelled() => return,
            changed = settings.changed() => {
                if changed.is_err() { return; }
                retry_delay = INITIAL_RETRY_DELAY;
            },
            _ = tokio::time::sleep(retry_delay) => {
                retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
            },
        }
    }
}

async fn ping_pong(conn: &Transport, last_pong: &Cell<Option<Instant>>) {
    let connected = Instant::now();
    let (hello, len) = ProtoEvent::Hello {
        commit: local_commit(),
    }
    .into();
    if conn.send(&hello[..len]).await.is_err() {
        return;
    }
    let (ping, len) = ProtoEvent::Ping.into();
    loop {
        if conn.send(&ping[..len]).await.is_err() {
            return;
        }
        tokio::time::sleep(HEARTBEAT_INTERVAL).await;
        if last_pong.get().unwrap_or(connected).elapsed() >= HEARTBEAT_TIMEOUT {
            log::warn!("peer did not respond; reconnecting");
            return;
        }
    }
}

async fn receive_loop(
    context: &ConnectionContext,
    handle: ClientHandle,
    conn: &Transport,
    last_pong: &Cell<Option<Instant>>,
) {
    let mut buf = [0u8; MAX_EVENT_SIZE];
    loop {
        let len = match conn.recv(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(len) => len,
        };
        // DTLS frames are independent; never decode leftovers from an older frame.
        buf[len..].fill(0);
        match buf.try_into() {
            Ok(ProtoEvent::Pong(enabled)) => {
                context.clients.set_alive(handle, enabled);
                last_pong.set(Some(Instant::now()));
            }
            Ok(ProtoEvent::Hello { commit }) => {
                context.clients.set_peer_commit(handle, Some(commit))
            }
            Ok(event) => {
                if context.tx.send((handle, event)).is_err() {
                    return;
                }
            }
            Err(error) => log::debug!("ignoring undecodable event: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns::DnsResolver;
    use futures::FutureExt;
    use tokio::sync::{Notify, mpsc};
    use webrtc_util::conn::conn_pipe::pipe;

    fn client() -> (ClientManager, ClientHandle) {
        let clients = ClientManager::default();
        let handle = clients.add_client();
        clients.set_hostname(handle, Some("peer.local".into()));
        clients.activate_client(handle);
        (clients, handle)
    }

    fn connection(clients: ClientManager, resolver: &DnsResolver) -> LanMouseConnection {
        LanMouseConnection::new(
            Certificate::generate_self_signed(Vec::new()).unwrap(),
            clients,
            resolver.lookup(),
        )
    }

    async fn drive_dns(mut resolver: DnsResolver, clients: ClientManager, stop: CancellationToken) {
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                event = resolver.event() => { event.apply(&clients); },
            }
        }
        resolver.terminate().await;
    }

    #[tokio::test]
    async fn eof_does_not_replay_the_last_received_event() {
        let (clients, handle) = client();
        let mut resolver = DnsResolver::new().unwrap();
        let mut connection = connection(clients, &resolver);
        let (reader, writer) = pipe();
        let reader: Transport = Arc::new(reader);
        let (data, len) = ProtoEvent::Leave(42).into();
        writer.send(&data[..len]).await.unwrap();
        writer.send(&[]).await.unwrap();
        let last_pong = Cell::new(None);
        tokio::time::timeout(
            Duration::from_secs(1),
            receive_loop(&connection.context, handle, &reader, &last_pong),
        )
        .await
        .expect("EOF did not stop the receiver");
        assert!(matches!(
            connection.recv().await,
            (_, ProtoEvent::Leave(42))
        ));
        assert!(
            connection.recv().now_or_never().is_none(),
            "EOF replayed input"
        );
        connection.terminate().await;
        resolver.terminate().await;
    }

    #[tokio::test]
    async fn old_transport_cleanup_preserves_a_new_connection_to_the_same_client() {
        let (clients, handle) = client();
        let mut resolver = DnsResolver::new().unwrap();
        let mut connection = connection(clients.clone(), &resolver);
        let (old, _old_peer) = pipe();
        let old: Transport = Arc::new(old);
        let (new, new_peer) = pipe();
        let new: Transport = Arc::new(new);
        let settings = clients.connection_changes(handle).unwrap();
        clients.set_active_addr(handle, Some("127.0.0.2:4242".parse().unwrap()));
        clients.set_alive(handle, true);
        connection.context.conns.borrow_mut().insert(
            handle,
            ActiveConnection {
                conn: new,
                failed: CancellationToken::new(),
                settings: settings.clone(),
            },
        );
        connection.context.disconnect(handle, &old, &settings).await;
        connection.send(ProtoEvent::Leave(7), handle).await.unwrap();
        let mut data = [0; MAX_EVENT_SIZE];
        new_peer.recv(&mut data).await.unwrap();
        assert!(matches!(
            ProtoEvent::try_from(data),
            Ok(ProtoEvent::Leave(7))
        ));
        assert_eq!(
            clients.get_state(handle).unwrap().1.active_addr,
            Some("127.0.0.2:4242".parse().unwrap())
        );
        connection.context.conns.borrow_mut().clear();
        connection.terminate().await;
        resolver.terminate().await;
    }

    #[tokio::test(start_paused = true)]
    async fn retries_refresh_dns_with_bounded_backoff_without_more_input_and_stop_when_disabled() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (clients, handle) = client();
                let (queries, mut query_times) = mpsc::unbounded_channel();
                let resolver = DnsResolver::with_lookup(move |_| {
                    queries.send(Instant::now()).unwrap();
                    std::future::ready(Ok(Vec::new()))
                });
                let mut connection = connection(clients.clone(), &resolver);
                let stop = CancellationToken::new();
                let dns = spawn_local(drive_dns(resolver, clients.clone(), stop.clone()));
                for _ in 0..20 {
                    assert!(connection.send(ProtoEvent::Ping, handle).await.is_err());
                }
                let mut previous = query_times.recv().await.unwrap();
                for delay in [1, 2, 4, 8, 16, 30, 30] {
                    let now = query_times.recv().await.unwrap();
                    assert_eq!(now.duration_since(previous), Duration::from_secs(delay));
                    previous = now;
                }
                clients.deactivate_client(handle);
                assert!(
                    tokio::time::timeout(Duration::from_secs(60), query_times.recv(),)
                        .await
                        .is_err(),
                    "disabled client kept resolving"
                );
                connection.terminate().await;
                stop.cancel();
                dns.await.unwrap();
            })
            .await;
    }

    struct LookupResource(Arc<Notify>);

    impl Drop for LookupResource {
        fn drop(&mut self) {
            self.0.notify_one();
        }
    }

    #[tokio::test]
    async fn removing_a_client_cancels_an_inflight_lookup() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (clients, handle) = client();
                let started = Arc::new(Notify::new());
                let dropped = Arc::new(Notify::new());
                let resolver = DnsResolver::with_lookup({
                    let started = started.clone();
                    let dropped = dropped.clone();
                    move |_| {
                        let started = started.clone();
                        let resource = LookupResource(dropped.clone());
                        async move {
                            let _resource = resource;
                            started.notify_one();
                            std::future::pending::<io::Result<Vec<std::net::IpAddr>>>().await
                        }
                    }
                });
                let mut connection = connection(clients.clone(), &resolver);
                let stop = CancellationToken::new();
                let dns = spawn_local(drive_dns(resolver, clients.clone(), stop.clone()));
                assert!(connection.send(ProtoEvent::Ping, handle).await.is_err());
                started.notified().await;
                clients.remove_client(handle);
                tokio::time::timeout(Duration::from_secs(1), dropped.notified())
                    .await
                    .expect("removed client retained its pending lookup");
                connection.terminate().await;
                stop.cancel();
                dns.await.unwrap();
            })
            .await;
    }

    #[cfg(target_os = "linux")]
    mod address_migration {
        use super::*;
        use std::net::IpAddr;
        use std::sync::Mutex;
        use webrtc_dtls::config::ClientAuthType::RequireAnyClientCert;
        use webrtc_util::conn::Listener;

        async fn server(ip: IpAddr, port: u16) -> impl Listener {
            webrtc_dtls::listener::listen(
                SocketAddr::new(ip, port),
                Config {
                    certificates: vec![Certificate::generate_self_signed(Vec::new()).unwrap()],
                    extended_master_secret: ExtendedMasterSecretType::Require,
                    client_auth: RequireAnyClientCert,
                    ..Default::default()
                },
            )
            .await
            .unwrap()
        }

        fn respond(
            conn: Transport,
            stop: CancellationToken,
        ) -> (JoinHandle<()>, mpsc::UnboundedReceiver<ProtoEvent>) {
            let (tx, rx) = mpsc::unbounded_channel();
            let task = spawn_local(async move {
                let mut data = [0; MAX_EVENT_SIZE];
                let mut announced = false;
                loop {
                    let len = tokio::select! {
                        _ = stop.cancelled() => break,
                        result = conn.recv(&mut data) => match result {
                            Ok(0) | Err(_) => break,
                            Ok(len) => len,
                        },
                    };
                    data[len..].fill(0);
                    match ProtoEvent::try_from(data).unwrap() {
                        ProtoEvent::Hello { .. } => {}
                        ProtoEvent::Ping => {
                            let (data, len) = ProtoEvent::Pong(true).into();
                            conn.send(&data[..len]).await.unwrap();
                            if !announced {
                                let (data, len) = ProtoEvent::Ack(42).into();
                                conn.send(&data[..len]).await.unwrap();
                                announced = true;
                            }
                        }
                        event => {
                            let _ = tx.send(event);
                        }
                    }
                }
                let _ = conn.close().await;
            });
            (task, rx)
        }

        #[tokio::test]
        async fn a_live_client_reconnects_to_a_changed_dns_address_without_user_input() {
            tokio::task::LocalSet::new()
                .run_until(async {
                    tokio::time::timeout(Duration::from_secs(15), async {
                        let old_ip = "127.0.0.1".parse().unwrap();
                        let new_ip = "127.0.0.2".parse().unwrap();
                        let old_server = server(old_ip, 0).await;
                        let port = old_server.addr().await.unwrap().port();
                        let new_server = server(new_ip, port).await;
                        let answer = Arc::new(Mutex::new(old_ip));
                        let resolver = DnsResolver::with_lookup({
                            let answer = answer.clone();
                            move |_| std::future::ready(Ok(vec![*answer.lock().unwrap()]))
                        });
                        let (clients, handle) = client();
                        clients.set_port(handle, port);
                        let mut connection = connection(clients.clone(), &resolver);
                        let dns_stop = CancellationToken::new();
                        let dns =
                            spawn_local(drive_dns(resolver, clients.clone(), dns_stop.clone()));
                        assert!(
                            connection
                                .send(ProtoEvent::Leave(999), handle)
                                .await
                                .is_err()
                        );
                        let (old_conn, _) = old_server.accept().await.unwrap();
                        let old_stop = CancellationToken::new();
                        let (old_task, _old_input) = respond(old_conn, old_stop.clone());
                        assert!(matches!(connection.recv().await, (_, ProtoEvent::Ack(42))));
                        assert_eq!(
                            clients
                                .get_state(handle)
                                .unwrap()
                                .1
                                .active_addr
                                .unwrap()
                                .ip(),
                            old_ip
                        );
                        *answer.lock().unwrap() = new_ip;
                        old_stop.cancel();
                        old_task.await.unwrap();
                        old_server.close().await.unwrap();
                        // No send/enter call here: transport loss must drive recovery.
                        let (new_conn, _) = new_server.accept().await.unwrap();
                        let new_stop = CancellationToken::new();
                        let (new_task, mut input) = respond(new_conn, new_stop.clone());
                        assert!(matches!(connection.recv().await, (_, ProtoEvent::Ack(42))));
                        assert_eq!(
                            clients
                                .get_state(handle)
                                .unwrap()
                                .1
                                .active_addr
                                .unwrap()
                                .ip(),
                            new_ip
                        );
                        connection.send(ProtoEvent::Leave(7), handle).await.unwrap();
                        assert!(matches!(input.recv().await, Some(ProtoEvent::Leave(7))));
                        connection.terminate().await;
                        new_stop.cancel();
                        new_task.await.unwrap();
                        new_server.close().await.unwrap();
                        dns_stop.cancel();
                        dns.await.unwrap();
                    })
                    .await
                    .expect("client did not reconnect after its address changed");
                })
                .await;
        }
    }
}
