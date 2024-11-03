use crate::listen::{LanMouseListener, ListenerCreationError};
use futures::StreamExt;
use input_emulation::{EmulationHandle, InputEmulation, InputEmulationError};
use input_event::Event;
use lan_mouse_proto::{Position, ProtoEvent};
use local_channel::mpsc::{channel, Receiver, Sender};
use std::{
    cell::Cell,
    collections::HashMap,
    net::SocketAddr,
    rc::Rc,
    time::{Duration, Instant},
};
use tokio::{
    select,
    task::{spawn_local, JoinHandle},
};

/// emulation handling events received from a listener
pub(crate) struct Emulation {
    task: JoinHandle<()>,
    request_tx: Sender<EmulationRequest>,
    event_rx: Receiver<EmulationEvent>,
}

pub(crate) enum EmulationEvent {
    /// new connection
    Connected {
        /// address of the connection
        addr: SocketAddr,
        /// position of the connection
        pos: lan_mouse_ipc::Position,
        /// certificate fingerprint of the connection
        fingerprint: String,
    },
    /// connection closed
    Disconnected { addr: SocketAddr },
    /// the port of the listener has changed
    PortChanged(Result<u16, ListenerCreationError>),
    /// emulation was disabled
    EmulationDisabled,
    /// emulation was enabled
    EmulationEnabled,
    /// capture should be released
    ReleaseNotify,
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
        let task = spawn_local(Self::run(listener, emulation_proxy, request_rx, event_tx));
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

    async fn run(
        mut listener: LanMouseListener,
        mut emulation_proxy: EmulationProxy,
        mut request_rx: Receiver<EmulationRequest>,
        event_tx: Sender<EmulationEvent>,
    ) {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        let mut last_response = HashMap::new();
        loop {
            select! {
                e = listener.next() =>  {
                    let (event, addr) = match e {
                        Some(e) => e,
                        None => break,
                    };
                    log::trace!("{event} <-<-<-<-<- {addr}");
                    last_response.insert(addr, Instant::now());
                    match event {
                        ProtoEvent::Enter(pos) => {
                            if let Some(fingerprint) = listener.get_certificate_fingerprint(addr).await {
                                log::info!("releasing capture: {addr} entered this device");
                                event_tx.send(EmulationEvent::ReleaseNotify).expect("channel closed");
                                listener.reply(addr, ProtoEvent::Ack(0)).await;
                                event_tx.send(EmulationEvent::Connected{addr, pos: to_ipc_pos(pos), fingerprint}).expect("channel closed");
                            }
                        }
                        ProtoEvent::Leave(_) => {
                            emulation_proxy.release_keys(addr);
                            listener.reply(addr, ProtoEvent::Ack(0)).await;
                        }
                        ProtoEvent::Input(event) => emulation_proxy.consume(event, addr),
                        ProtoEvent::Ping => listener.reply(addr, ProtoEvent::Pong(emulation_proxy.emulation_active.get())).await,
                        _ => {}
                    }
                }
                event = emulation_proxy.event() => {
                    event_tx.send(event).expect("channel closed");
                }
                request = request_rx.recv() => match request.expect("channel closed") {
                    // reenable emulation
                    EmulationRequest::Reenable => emulation_proxy.reenable(),
                    // notify the other end that we hit a barrier (should release capture)
                    EmulationRequest::Release(addr) => listener.reply(addr, ProtoEvent::Leave(0)).await,
                    EmulationRequest::ChangePort(port) => {
                        listener.request_port_change(port);
                        let result = listener.port_changed().await;
                        event_tx.send(EmulationEvent::PortChanged(result)).expect("channel closed");
                    }
                    EmulationRequest::Terminate => break,
                },
                _ = interval.tick() => {
                    last_response.retain(|&addr,instant| {
                        if instant.elapsed() > Duration::from_secs(5) {
                            log::warn!("releasing keys: {addr} not responding!");
                            emulation_proxy.release_keys(addr);
                            event_tx.send(EmulationEvent::Disconnected { addr }).expect("channel closed");
                            false
                        } else {
                            true
                        }
                    });
                }
            }
        }
        listener.terminate().await;
        emulation_proxy.terminate().await;
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
    ReleaseKeys(SocketAddr),
    Terminate,
    Reenable,
}

impl EmulationProxy {
    fn new(backend: Option<input_emulation::Backend>) -> Self {
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let emulation_active = Rc::new(Cell::new(false));
        let exit_requested = Rc::new(Cell::new(false));
        let task = spawn_local(Self::emulation_task(
            backend,
            exit_requested.clone(),
            request_rx,
            event_tx,
        ));
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

    fn release_keys(&self, addr: SocketAddr) {
        self.request_tx
            .send(ProxyRequest::ReleaseKeys(addr))
            .expect("channel closed");
    }

    async fn emulation_task(
        backend: Option<input_emulation::Backend>,
        exit_requested: Rc<Cell<bool>>,
        mut request_rx: Receiver<ProxyRequest>,
        event_tx: Sender<EmulationEvent>,
    ) {
        let mut handles = HashMap::new();
        let mut next_id = 0;
        loop {
            if let Err(e) = Self::do_emulation(
                backend,
                &mut handles,
                &mut next_id,
                &mut request_rx,
                &event_tx,
            )
            .await
            {
                log::warn!("input emulation exited: {e}");
            }
            if exit_requested.get() {
                break;
            }
            // wait for reenable request
            loop {
                match request_rx.recv().await.expect("channel closed") {
                    ProxyRequest::Reenable => break,
                    ProxyRequest::Terminate => return,
                    ProxyRequest::Input(..) => { /* emulation inactive => ignore */ }
                    ProxyRequest::ReleaseKeys(..) => { /* emulation inactive => ignore */ }
                }
            }
        }
    }

    async fn do_emulation(
        backend: Option<input_emulation::Backend>,
        handles: &mut HashMap<SocketAddr, EmulationHandle>,
        next_id: &mut EmulationHandle,
        request_rx: &mut Receiver<ProxyRequest>,
        event_tx: &Sender<EmulationEvent>,
    ) -> Result<(), InputEmulationError> {
        log::info!("creating input emulation ...");
        let mut emulation = tokio::select! {
            r = InputEmulation::new(backend) => r?,
            // allow termination event while requesting input emulation
            _ = wait_for_termination(request_rx) => return Ok(()),
        };

        // used to send enabled and disabled events
        let _emulation_guard = DropGuard::new(
            event_tx,
            EmulationEvent::EmulationEnabled,
            EmulationEvent::EmulationDisabled,
        );

        // create active handles
        if let Err(e) =
            Self::create_clients(&mut emulation, handles.values().copied(), request_rx).await
        {
            emulation.terminate().await;
            return Err(e);
        }

        let res = Self::do_emulation_session(&mut emulation, handles, next_id, request_rx).await;
        // FIXME replace with async drop when stabilized
        emulation.terminate().await;
        res
    }

    async fn create_clients(
        emulation: &mut InputEmulation,
        handles: impl Iterator<Item = EmulationHandle>,
        request_rx: &mut Receiver<ProxyRequest>,
    ) -> Result<(), InputEmulationError> {
        for handle in handles {
            tokio::select! {
                _ = emulation.create(handle) => {},
                _ = wait_for_termination(request_rx) => return Ok(()),
            }
        }
        Ok(())
    }

    async fn do_emulation_session(
        emulation: &mut InputEmulation,
        handles: &mut HashMap<SocketAddr, EmulationHandle>,
        next_id: &mut EmulationHandle,
        rx: &mut Receiver<ProxyRequest>,
    ) -> Result<(), InputEmulationError> {
        loop {
            tokio::select! {
                e = rx.recv() => match e.expect("channel closed") {
                    ProxyRequest::Input(event, addr) => {
                        let handle = match handles.get(&addr) {
                            Some(&handle) => handle,
                            None => {
                                let handle = *next_id;
                                *next_id += 1;
                                emulation.create(handle).await;
                                handles.insert(addr, handle);
                                handle
                            }
                        };
                        emulation.consume(event, handle).await?;
                    },
                    ProxyRequest::ReleaseKeys(addr) => {
                        if let Some(&handle) = handles.get(&addr) {
                            emulation.release_keys(handle).await?
                        }
                    }
                    ProxyRequest::Terminate => break Ok(()),
                    ProxyRequest::Reenable => continue,
                },
            }
        }
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
            ProxyRequest::ReleaseKeys(_) => continue,
            ProxyRequest::Reenable => continue,
        }
    }
}

struct DropGuard<'a, T> {
    tx: &'a Sender<T>,
    on_drop: Option<T>,
}

impl<'a, T> DropGuard<'a, T> {
    fn new(tx: &'a Sender<T>, on_new: T, on_drop: T) -> Self {
        tx.send(on_new).expect("channel closed");
        let on_drop = Some(on_drop);
        Self { tx, on_drop }
    }
}

impl<'a, T> Drop for DropGuard<'a, T> {
    fn drop(&mut self) {
        self.tx
            .send(self.on_drop.take().expect("item"))
            .expect("channel closed");
    }
}
