use crate::config::local_commit;
use crate::listen::{LanMouseListener, ListenEvent, ListenerCreationError};
use futures::StreamExt;
use input_emulation::{EmulationHandle, InputEmulation, InputEmulationError};
use input_event::Event;
use lan_mouse_proto::{Position, ProtoEvent};
use local_channel::mpsc::{Receiver, Sender, channel};
use std::{
    cell::Cell,
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    rc::Rc,
    time::{Duration, Instant},
};
use tokio::{
    select,
    task::{JoinHandle, spawn_local},
};

/// emulation handling events received from a listener
pub(crate) struct Emulation {
    task: JoinHandle<()>,
    request_tx: Sender<EmulationRequest>,
    event_rx: Receiver<EmulationEvent>,
}

pub(crate) enum EmulationEvent {
    Connected {
        addr: SocketAddr,
        fingerprint: String,
    },
    ConnectionAttempt {
        fingerprint: String,
    },
    /// new connection
    Entered {
        /// address of the connection
        addr: SocketAddr,
        /// position of the connection
        pos: lan_mouse_ipc::Position,
        /// certificate fingerprint of the connection
        fingerprint: String,
    },
    /// connection closed
    Disconnected {
        addr: SocketAddr,
    },
    /// the port of the listener has changed
    PortChanged(Result<u16, ListenerCreationError>),
    /// emulation was disabled
    EmulationDisabled,
    /// emulation was enabled
    EmulationEnabled,
    /// capture should be released
    ReleaseNotify,
    /// peer sent us a Hello with its build commit hash. Used to
    /// populate `client_manager.peer_commit` from the listen side
    /// too — without this, peer-version visibility silently fails
    /// whenever the outgoing connection in the *other* direction is
    /// broken (one-way setups, asymmetric NAT, peer's TCP listener
    /// down). The connect-side path stays as the primary source;
    /// this is the defensive fallback.
    PeerHello {
        addr: SocketAddr,
        commit: [u8; 8],
    },
}

enum EmulationRequest {
    Reenable,
    Release(SocketAddr),
    ChangePort(u16),
    Terminate,
}

impl Emulation {
    pub(crate) fn new(
        backend: Option<input_emulation::Backend>,
        listener: LanMouseListener,
    ) -> Self {
        let emulation_proxy = EmulationProxy::new(backend);
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let emulation_task = ListenTask {
            listener,
            emulation_proxy,
            request_rx,
            event_tx,
        };
        let task = spawn_local(emulation_task.run());
        Self {
            task,
            request_tx,
            event_rx,
        }
    }

    pub(crate) fn send_leave_event(&self, addr: SocketAddr) {
        self.request_tx
            .send(EmulationRequest::Release(addr))
            .expect("channel closed");
    }

    pub(crate) fn reenable(&self) {
        self.request_tx
            .send(EmulationRequest::Reenable)
            .expect("channel closed");
    }

    pub(crate) fn request_port_change(&self, port: u16) {
        self.request_tx
            .send(EmulationRequest::ChangePort(port))
            .expect("channel closed")
    }

    pub(crate) async fn event(&mut self) -> EmulationEvent {
        self.event_rx.recv().await.expect("channel closed")
    }

    /// wait for termination
    pub(crate) async fn terminate(&mut self) {
        log::debug!("terminating emulation");
        self.request_tx
            .send(EmulationRequest::Terminate)
            .expect("channel closed");
        if let Err(e) = (&mut self.task).await {
            log::warn!("{e}");
        }
    }
}

struct ListenTask {
    listener: LanMouseListener,
    emulation_proxy: EmulationProxy,
    request_rx: Receiver<EmulationRequest>,
    event_tx: Sender<EmulationEvent>,
}

impl ListenTask {
    async fn run(mut self) {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        let mut last_response = HashMap::new();
        let mut rejected_connections = HashMap::new();
        loop {
            select! {
                e = self.listener.next() => {match e {
                    Some(ListenEvent::Msg { event, addr }) => {
                        log::trace!("{event} <-<-<-<-<- {addr}");
                        last_response.insert(addr, Instant::now());
                        match event {
                            ProtoEvent::Enter(pos) => {
                                if let Some(fingerprint) = self.listener.get_certificate_fingerprint(addr).await {
                                    log::info!("releasing capture: {addr} entered this device");
                                    self.event_tx.send(EmulationEvent::ReleaseNotify).expect("channel closed");
                                    self.listener.reply(addr, ProtoEvent::Ack(0)).await;
                                    self.event_tx.send(EmulationEvent::Entered{addr, pos: to_ipc_pos(pos), fingerprint}).expect("channel closed");
                                }
                            }
                            ProtoEvent::Leave(_) => {
                                self.emulation_proxy.remove(addr);
                                self.listener.reply(addr, ProtoEvent::Ack(0)).await;
                            }
                            ProtoEvent::Input(event) => self.emulation_proxy.consume(event, addr),
                            ProtoEvent::Ping => self.listener.reply(addr, ProtoEvent::Pong(self.emulation_proxy.emulation_active.get())).await,
                            // Peer's version handshake. Echo our own
                            // commit back so the peer's connect-side
                            // receive_loop populates its `peer_commit`,
                            // AND publish a PeerHello upward so our
                            // service can populate ours from the listen
                            // side too — the connect side is the primary
                            // path, but if the outbound direction is
                            // broken (one-way setup, NAT, peer's TCP
                            // listener down) the version display would
                            // otherwise silently say "unknown" while
                            // the peer is in fact happily talking to us.
                            ProtoEvent::Hello { commit } => {
                                self.listener.reply(addr, ProtoEvent::Hello { commit: local_commit() }).await;
                                self.event_tx.send(EmulationEvent::PeerHello { addr, commit }).expect("channel closed");
                            }
                            _ => {}
                        }
                    }
                    Some(ListenEvent::Accept { addr, fingerprint }) => {
                        self.event_tx.send(EmulationEvent::Connected { addr, fingerprint }).expect("channel closed");
                    }
                    Some(ListenEvent::Rejected { fingerprint }) => {
                        if rejected_connections.insert(fingerprint.clone(), Instant::now())
                            .is_none_or(|i| i.elapsed() >= Duration::from_secs(2)) {
                                self.event_tx.send(EmulationEvent::ConnectionAttempt { fingerprint }).expect("channel closed");
                            }
                    }
                    None => break
                }}
                event = self.emulation_proxy.event() => {
                    if matches!(&event, EmulationEvent::EmulationDisabled) {
                        // Release the source immediately; ping state also keeps
                        // it from entering while the backend is being rebuilt.
                        for &addr in last_response.keys() {
                            self.listener.reply(addr, ProtoEvent::Leave(0)).await;
                        }
                    }
                    self.event_tx.send(event).expect("channel closed");
                }
                request = self.request_rx.recv() => match request.expect("channel closed") {
                    // reenable emulation
                    EmulationRequest::Reenable => self.emulation_proxy.reenable(),
                    // notify the other end that we hit a barrier (should release capture)
                    EmulationRequest::Release(addr) => self.listener.reply(addr, ProtoEvent::Leave(0)).await,
                    EmulationRequest::ChangePort(port) => {
                        self.listener.request_port_change(port);
                        let result = self.listener.port_changed().await;
                        self.event_tx.send(EmulationEvent::PortChanged(result)).expect("channel closed");
                    }
                    EmulationRequest::Terminate => break,
                },
                _ = interval.tick() => {
                    last_response.retain(|&addr,instant| {
                        if instant.elapsed() > Duration::from_secs(1) {
                            log::warn!("releasing keys: {addr} not responding!");
                            self.emulation_proxy.remove(addr);
                            self.event_tx.send(EmulationEvent::Disconnected { addr }).expect("channel closed");
                            false
                        } else {
                            true
                        }
                    });
                }
            }
        }
        self.listener.terminate().await;
        self.emulation_proxy.terminate().await;
    }
}

/// proxy handling the actual input emulation,
/// discarding events when it is disabled
pub(crate) struct EmulationProxy {
    emulation_active: Rc<Cell<bool>>,
    exit_requested: Rc<Cell<bool>>,
    request_tx: Sender<ProxyRequest>,
    event_rx: Receiver<EmulationEvent>,
    task: JoinHandle<()>,
}

enum ProxyRequest {
    Input(Event, SocketAddr),
    Remove(SocketAddr),
    Terminate,
    Reenable,
}

impl EmulationProxy {
    fn new(backend: Option<input_emulation::Backend>) -> Self {
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let emulation_active = Rc::new(Cell::new(false));
        let exit_requested = Rc::new(Cell::new(false));
        let emulation_task = EmulationTask {
            backend,
            exit_requested: exit_requested.clone(),
            request_rx,
            event_tx,
            handles: Default::default(),
            next_id: 0,
        };
        let task = spawn_local(emulation_task.run());
        Self {
            emulation_active,
            exit_requested,
            request_tx,
            task,
            event_rx,
        }
    }

    async fn event(&mut self) -> EmulationEvent {
        let event = self.event_rx.recv().await.expect("channel closed");
        if let EmulationEvent::EmulationEnabled = event {
            self.emulation_active.replace(true);
        }
        if let EmulationEvent::EmulationDisabled = event {
            self.emulation_active.replace(false);
        }
        event
    }

    fn consume(&self, event: Event, addr: SocketAddr) {
        // ignore events if emulation is currently disabled
        if self.emulation_active.get() {
            self.request_tx
                .send(ProxyRequest::Input(event, addr))
                .expect("channel closed");
        }
    }

    fn remove(&self, addr: SocketAddr) {
        self.request_tx
            .send(ProxyRequest::Remove(addr))
            .expect("channel closed");
    }

    fn reenable(&self) {
        self.request_tx
            .send(ProxyRequest::Reenable)
            .expect("channel closed");
    }

    async fn terminate(&mut self) {
        self.exit_requested.replace(true);
        self.request_tx
            .send(ProxyRequest::Terminate)
            .expect("channel closed");
        let _ = (&mut self.task).await;
    }
}

struct EmulationTask {
    backend: Option<input_emulation::Backend>,
    exit_requested: Rc<Cell<bool>>,
    request_rx: Receiver<ProxyRequest>,
    event_tx: Sender<EmulationEvent>,
    handles: HashMap<SocketAddr, EmulationHandle>,
    next_id: EmulationHandle,
}

impl EmulationTask {
    async fn run(mut self) {
        let mut recoveries = VecDeque::new();
        loop {
            let result = self.do_emulation().await;
            if let Err(e) = &result {
                log::warn!("input emulation exited: {e}");
            }
            if self.exit_requested.get() {
                break;
            }
            // Old devices have been destroyed. Do not recreate their handles or
            // replay input that arrived before the source released its capture.
            self.handles.clear();
            let retry = result.as_ref().err().is_some_and(|e| {
                e.is_recoverable_output_stall()
                    && allow_output_recovery(&mut recoveries, Instant::now())
            });
            let delay = retry.then_some(Duration::from_secs(2));
            if retry {
                log::warn!(
                    "Wayland output stalled; releasing input and rebuilding emulation after 2s"
                );
            }
            if !wait_for_restart(&mut self.request_rx, delay).await {
                return;
            }
            if !retry {
                // An explicit reenable starts a fresh recovery budget.
                recoveries.clear();
            }
        }
    }

    async fn do_emulation(&mut self) -> Result<(), InputEmulationError> {
        log::info!("creating input emulation ...");
        let mut emulation = tokio::select! {
            r = InputEmulation::new(self.backend) => r?,
            // allow termination event while requesting input emulation
            _ = wait_for_termination(&mut self.request_rx) => return Ok(()),
        };

        // used to send enabled and disabled events
        let _emulation_guard = DropGuard::new(
            self.event_tx.clone(),
            EmulationEvent::EmulationEnabled,
            EmulationEvent::EmulationDisabled,
        );

        // create active handles
        if let Err(e) = self.create_clients(&mut emulation).await {
            emulation.terminate().await;
            return Err(e);
        }

        let res = self.do_emulation_session(&mut emulation).await;
        // FIXME replace with async drop when stabilized
        emulation.terminate().await;
        res
    }

    async fn create_clients(
        &mut self,
        emulation: &mut InputEmulation,
    ) -> Result<(), InputEmulationError> {
        for handle in self.handles.values() {
            tokio::select! {
                _ = emulation.create(*handle) => {},
                _ = wait_for_termination(&mut self.request_rx) => return Ok(()),
            }
        }
        Ok(())
    }

    async fn do_emulation_session(
        &mut self,
        emulation: &mut InputEmulation,
    ) -> Result<(), InputEmulationError> {
        loop {
            tokio::select! {
                e = self.request_rx.recv() => match e.expect("channel closed") {
                    ProxyRequest::Input(event, addr) => {
                        let handle = match self.handles.get(&addr) {
                            Some(&handle) => handle,
                            None => {
                                let handle = self.next_id;
                                self.next_id += 1;
                                emulation.create(handle).await;
                                self.handles.insert(addr, handle);
                                handle
                            }
                        };
                        emulation.consume(event, handle).await?;
                    },
                    ProxyRequest::Remove(addr) => {
                        if let Some(handle) = self.handles.remove(&addr) {
                            emulation.destroy(handle).await;
                        }
                    }
                    ProxyRequest::Terminate => break Ok(()),
                    ProxyRequest::Reenable => continue,
                },
            }
        }
    }
}

fn allow_output_recovery(attempts: &mut VecDeque<Instant>, now: Instant) -> bool {
    while attempts
        .front()
        .is_some_and(|last| now.duration_since(*last) >= Duration::from_secs(60))
    {
        attempts.pop_front();
    }
    if attempts.len() >= 3 {
        return false;
    }
    attempts.push_back(now);
    true
}

async fn wait_for_restart(rx: &mut Receiver<ProxyRequest>, delay: Option<Duration>) -> bool {
    let resume = async {
        match delay {
            Some(delay) => tokio::time::sleep(delay).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(resume);
    let mut explicitly_requested = false;
    loop {
        tokio::select! {
            // Drain stale input even when the retry timer has already expired.
            biased;
            request = rx.recv() => match request {
                Some(ProxyRequest::Terminate) | None => return false,
                Some(ProxyRequest::Reenable) => explicitly_requested = true,
                Some(ProxyRequest::Input(..) | ProxyRequest::Remove(..)) => {},
            },
            _ = &mut resume => return true,
            _ = std::future::ready(()), if explicitly_requested => return true,
        }
    }
}

fn to_ipc_pos(pos: Position) -> lan_mouse_ipc::Position {
    match pos {
        Position::Left => lan_mouse_ipc::Position::Left,
        Position::Right => lan_mouse_ipc::Position::Right,
        Position::Top => lan_mouse_ipc::Position::Top,
        Position::Bottom => lan_mouse_ipc::Position::Bottom,
    }
}

async fn wait_for_termination(rx: &mut Receiver<ProxyRequest>) {
    loop {
        match rx.recv().await.expect("channel closed") {
            ProxyRequest::Terminate => return,
            ProxyRequest::Input(_, _) => continue,
            ProxyRequest::Remove(_) => continue,
            ProxyRequest::Reenable => continue,
        }
    }
}

struct DropGuard<T> {
    tx: Sender<T>,
    on_drop: Option<T>,
}

impl<T> DropGuard<T> {
    fn new(tx: Sender<T>, on_new: T, on_drop: T) -> Self {
        tx.send(on_new).expect("channel closed");
        let on_drop = Some(on_drop);
        Self { tx, on_drop }
    }
}

impl<T> Drop for DropGuard<T> {
    fn drop(&mut self) {
        self.tx
            .send(self.on_drop.take().expect("item"))
            .expect("channel closed");
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[tokio::test]
    async fn recovery_discards_buffered_input_before_restart() {
        let (tx, mut rx) = channel();
        let addr = "127.0.0.1:4242".parse().unwrap();
        let press = Event::Keyboard(input_event::KeyboardEvent::Key {
            time: 0,
            key: 30,
            state: 1,
        });
        tx.send(ProxyRequest::Input(press, addr)).unwrap();
        tx.send(ProxyRequest::Reenable).unwrap();
        tx.send(ProxyRequest::Input(press, addr)).unwrap();
        tx.send(ProxyRequest::Remove(addr)).unwrap();
        assert!(wait_for_restart(&mut rx, Some(Duration::ZERO)).await);
        // A replayed key would be delivered before this termination request.
        tx.send(ProxyRequest::Terminate).unwrap();
        assert!(matches!(rx.recv().await, Some(ProxyRequest::Terminate)));
    }

    #[tokio::test]
    async fn termination_interrupts_recovery_wait() {
        let (tx, mut rx) = channel();
        tx.send(ProxyRequest::Terminate).unwrap();
        assert!(
            !tokio::time::timeout(
                Duration::from_millis(100),
                wait_for_restart(&mut rx, Some(Duration::from_secs(60)))
            )
            .await
            .unwrap()
        );
    }

    #[test]
    fn repeated_stalls_have_a_bounded_recovery_budget() {
        let now = Instant::now();
        let mut attempts = VecDeque::new();
        for _ in 0..3 {
            assert!(allow_output_recovery(&mut attempts, now));
        }
        assert!(!allow_output_recovery(
            &mut attempts,
            now + Duration::from_secs(59)
        ));
        assert!(allow_output_recovery(
            &mut attempts,
            now + Duration::from_secs(60)
        ));
    }

    #[cfg(wlroots_emulation)]
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires a real Wayland compositor and isolated Wayland stall proxy"]
    async fn real_wayland_stall_disables_then_rebuilds_emulation() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let flag =
                    std::env::var("LAN_MOUSE_STALL_FLAG").expect("isolated injector flag required");
                let mut proxy = EmulationProxy::new(Some(input_emulation::Backend::Wlroots));
                assert!(matches!(
                    tokio::time::timeout(Duration::from_secs(3), proxy.event())
                        .await
                        .unwrap(),
                    EmulationEvent::EmulationEnabled
                ));
                std::fs::write(&flag, b"once").unwrap();
                let addr = "127.0.0.1:4242".parse().unwrap();
                let motion = Event::Pointer(input_event::PointerEvent::Motion {
                    time: 0,
                    dx: 0.0,
                    dy: 0.0,
                });
                for _ in 0..2000 {
                    proxy.consume(motion, addr);
                }
                assert!(matches!(
                    tokio::time::timeout(Duration::from_secs(3), proxy.event())
                        .await
                        .unwrap(),
                    EmulationEvent::EmulationDisabled
                ));
                assert!(matches!(
                    tokio::time::timeout(Duration::from_secs(5), proxy.event())
                        .await
                        .unwrap(),
                    EmulationEvent::EmulationEnabled
                ));
                proxy.consume(motion, addr);
                // The rebuilt backend must accept new input, not immediately disable again.
                assert!(
                    tokio::time::timeout(Duration::from_millis(800), proxy.event())
                        .await
                        .is_err()
                );
                proxy.terminate().await;
                assert!(!std::path::Path::new(&flag).exists());
            })
            .await;
    }
}
