use emulation_task::EmulationEvent;
use log;
use std::{
    cell::{Cell, RefCell},
    collections::{HashSet, VecDeque},
    io::ErrorKind,
    net::{IpAddr, SocketAddr},
    rc::Rc,
};
use tokio::{
    io::ReadHalf,
    join, signal,
    sync::{
        mpsc::{channel, Sender},
        Notify,
    },
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    client::{ClientConfig, ClientHandle, ClientManager, ClientState, Position},
    config::Config,
    dns::DnsResolver,
    frontend::{self, FrontendEvent, FrontendListener, FrontendRequest, Status},
    server::capture_task::CaptureEvent,
};

#[cfg(unix)]
use tokio::net::UnixStream;

#[cfg(windows)]
use tokio::net::TcpStream;

mod capture_task;
mod emulation_task;
mod network_task;
mod ping_task;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    /// Currently sending events to another device
    Sending,
    /// Currently receiving events from other devices
    Receiving,
    /// Entered the deadzone of another device but waiting
    /// for acknowledgement (Leave event) from the device
    AwaitingLeave,
}

#[derive(Clone)]
pub struct Server {
    active_client: Rc<Cell<Option<ClientHandle>>>,
    pub(crate) client_manager: Rc<RefCell<ClientManager>>,
    port: Rc<Cell<u16>>,
    state: Rc<Cell<State>>,
    release_bind: Vec<input_event::scancode::Linux>,
    notifies: Rc<Notifies>,
    config: Rc<Config>,
    pending_frontend_events: Rc<RefCell<VecDeque<FrontendEvent>>>,
    pending_dns_requests: Rc<RefCell<VecDeque<ClientHandle>>>,
    capture_status: Rc<Cell<Status>>,
    emulation_status: Rc<Cell<Status>>,
}

#[derive(Default)]
struct Notifies {
    capture: Notify,
    emulation: Notify,
    ping: Notify,
    port_changed: Notify,
    frontend_event_pending: Notify,
    dns_request_pending: Notify,
    cancel: CancellationToken,
}

impl Server {
    pub fn new(config: Config) -> Self {
        let active_client = Rc::new(Cell::new(None));
        let client_manager = Rc::new(RefCell::new(ClientManager::default()));
        let state = Rc::new(Cell::new(State::Receiving));
        let port = Rc::new(Cell::new(config.port));
        for config_client in config.get_clients() {
            let client = ClientConfig {
                hostname: config_client.hostname,
                fix_ips: config_client.ips.into_iter().collect(),
                port: config_client.port,
                pos: config_client.pos,
                cmd: config_client.enter_hook,
            };
            let state = ClientState {
                active: config_client.active,
                ips: HashSet::from_iter(client.fix_ips.iter().cloned()),
                ..Default::default()
            };
            let mut client_manager = client_manager.borrow_mut();
            let handle = client_manager.add_client();
            let c = client_manager.get_mut(handle).expect("invalid handle");
            *c = (client, state);
        }

        // task notification tokens
        let notifies = Rc::new(Notifies::default());
        let release_bind = config.release_bind.clone();

        let config = Rc::new(config);

        Self {
            config,
            active_client,
            client_manager,
            port,
            state,
            release_bind,
            notifies,
            pending_frontend_events: Rc::new(RefCell::new(VecDeque::new())),
            pending_dns_requests: Rc::new(RefCell::new(VecDeque::new())),
            capture_status: Default::default(),
            emulation_status: Default::default(),
        }
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        // create frontend communication adapter, exit if already running
        let mut frontend = match FrontendListener::new().await {
            Some(f) => f?,
            None => {
                log::info!("service already running, exiting");
                return Ok(());
            }
        };

        let (capture_tx, capture_rx) = channel(1); /* requests for input capture */
        let (emulation_tx, emulation_rx) = channel(1); /* emulation requests */
        let (udp_recv_tx, udp_recv_rx) = channel(1); /* udp receiver */
        let (udp_send_tx, udp_send_rx) = channel(1); /* udp sender */
        let (request_tx, mut request_rx) = channel(1); /* frontend requests */

        // udp task
        let network = network_task::new(self.clone(), udp_recv_tx.clone(), udp_send_rx).await?;

        // input capture
        let capture = capture_task::new(self.clone(), capture_rx, udp_send_tx.clone());

        // input emulation
        let emulation = emulation_task::new(
            self.clone(),
            emulation_rx,
            udp_recv_rx,
            udp_send_tx.clone(),
            capture_tx.clone(),
        );

        // create dns resolver
        let (resolver, dns_request) = DnsResolver::new().await?;
        let server = self.clone();
        let dns_task = tokio::task::spawn_local(async move {
            resolver.run(server).await;
        });

        // task that pings clients to see if they are responding
        let ping = ping_task::new(
            self.clone(),
            udp_send_tx.clone(),
            emulation_tx.clone(),
            capture_tx.clone(),
        );

        for handle in self.active_clients() {
            self.request_dns(handle);
        }

        log::info!("running service");

        let mut join_handles = vec![];

        loop {
            tokio::select! {
                stream = frontend.accept() => {
                    match stream {
                        Ok(s) => join_handles.push(handle_frontend_stream(self.notifies.cancel.clone(), s, request_tx.clone())),
                        Err(e) => log::warn!("error accepting frontend connection: {e}"),
                    };
                    self.enumerate();
                    self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status.get()));
                    self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status.get()));
                    self.notify_frontend(FrontendEvent::PortChanged(self.port.get(), None));
                }
                request = request_rx.recv() => {
                    let request = request.expect("channel closed");
                    log::debug!("received frontend request: {request:?}");
                    self.handle_request(&capture_tx.clone(), &emulation_tx.clone(), request).await;
                    log::debug!("handled frontend request");
                }
                _ = self.notifies.frontend_event_pending.notified() => {
                    loop {
                        let event = self.pending_frontend_events.borrow_mut().pop_front();
                        if let Some(event) = event {
                            frontend.broadcast(event).await;
                        } else {
                            break;
                        }
                    }
                },
                _ = self.notifies.dns_request_pending.notified() => {
                    loop {
                        let request = self.pending_dns_requests.borrow_mut().pop_front();
                        if let Some(request) = request {
                            dns_request.send(request).await.expect("channel closed");
                        } else {
                            break;
                        }
                    }
                }
                _ = self.cancelled() => break,
                r = signal::ctrl_c() => {
                    r.expect("failed to wait for CTRL+C");
                    break;
                }
            }
        }

        log::info!("terminating service");

        assert!(!capture_tx.is_closed());
        assert!(!emulation_tx.is_closed());
        assert!(!udp_recv_tx.is_closed());
        assert!(!udp_send_tx.is_closed());
        assert!(!request_tx.is_closed());
        assert!(!dns_request.is_closed());

        self.cancel();
        futures::future::join_all(join_handles).await;
        let _ = join!(capture, dns_task, emulation, network, ping);

        Ok(())
    }

    fn notify_frontend(&self, event: FrontendEvent) {
        self.pending_frontend_events.borrow_mut().push_back(event);
        self.notifies.frontend_event_pending.notify_one();
    }

    fn cancel(&self) {
        self.notifies.cancel.cancel();
    }

    pub(crate) async fn cancelled(&self) {
        self.notifies.cancel.cancelled().await
    }

    fn is_cancelled(&self) -> bool {
        self.notifies.cancel.is_cancelled()
    }

    fn notify_capture(&self) {
        self.notifies.capture.notify_waiters()
    }

    async fn capture_notified(&self) {
        self.notifies.capture.notified().await
    }

    fn notify_emulation(&self) {
        self.notifies.emulation.notify_waiters()
    }

    async fn emulation_notified(&self) {
        self.notifies.emulation.notified().await
    }

    fn restart_ping_timer(&self) {
        self.notifies.ping.notify_waiters()
    }

    async fn ping_timer_notified(&self) {
        self.notifies.ping.notified().await
    }

    fn request_port_change(&self, port: u16) {
        self.port.replace(port);
        self.notifies.port_changed.notify_one();
    }

    fn notify_port_changed(&self, port: u16, msg: Option<String>) {
        self.port.replace(port);
        self.notify_frontend(FrontendEvent::PortChanged(port, msg));
    }

    pub(crate) fn client_resolved(&self, handle: ClientHandle) {
        let state = self.client_manager.borrow().get(handle).cloned();
        if let Some((config, state)) = state {
            self.notify_frontend(FrontendEvent::State(handle, config, state));
        }
    }

    fn active_clients(&self) -> Vec<ClientHandle> {
        self.client_manager
            .borrow()
            .get_client_states()
            .filter_map(|(h, (_, s))| if s.active { Some(h) } else { None })
            .collect()
    }

    fn request_dns(&self, handle: ClientHandle) {
        self.pending_dns_requests.borrow_mut().push_back(handle);
        self.notifies.dns_request_pending.notify_one();
    }

    async fn handle_request(
        &self,
        capture: &Sender<CaptureEvent>,
        emulate: &Sender<EmulationEvent>,
        event: FrontendRequest,
    ) -> bool {
        log::debug!("frontend: {event:?}");
        match event {
            FrontendRequest::EnableCapture => {
                log::info!("received capture enable request");
                self.notify_capture();
            }
            FrontendRequest::EnableEmulation => {
                log::info!("received emulation enable request");
                self.notify_emulation();
            }
            FrontendRequest::Create => {
                let handle = self.add_client().await;
                self.request_dns(handle);
            }
            FrontendRequest::Activate(handle, active) => {
                if active {
                    self.activate_client(capture, emulate, handle).await;
                } else {
                    self.deactivate_client(capture, emulate, handle).await;
                }
            }
            FrontendRequest::ChangePort(port) => self.request_port_change(port),
            FrontendRequest::Delete(handle) => {
                self.remove_client(capture, emulate, handle).await;
                self.notify_frontend(FrontendEvent::Deleted(handle));
            }
            FrontendRequest::Enumerate() => self.enumerate(),
            FrontendRequest::GetState(handle) => self.broadcast_client(handle),
            FrontendRequest::UpdateFixIps(handle, fix_ips) => {
                self.update_fix_ips(handle, fix_ips);
                self.request_dns(handle);
            }
            FrontendRequest::UpdateHostname(handle, host) => self.update_hostname(handle, host),
            FrontendRequest::UpdatePort(handle, port) => self.update_port(handle, port),
            FrontendRequest::UpdatePosition(handle, pos) => {
                self.update_pos(handle, capture, emulate, pos).await;
            }
            FrontendRequest::ResolveDns(handle) => self.request_dns(handle),
        };
        false
    }

    fn enumerate(&self) {
        let clients = self
            .client_manager
            .borrow()
            .get_client_states()
            .map(|(h, (c, s))| (h, c.clone(), s.clone()))
            .collect();
        self.notify_frontend(FrontendEvent::Enumerate(clients));
    }

    async fn add_client(&self) -> ClientHandle {
        let handle = self.client_manager.borrow_mut().add_client();
        log::info!("added client {handle}");
        let (c, s) = self.client_manager.borrow().get(handle).unwrap().clone();
        self.notify_frontend(FrontendEvent::Created(handle, c, s));
        handle
    }

    async fn deactivate_client(
        &self,
        capture: &Sender<CaptureEvent>,
        emulate: &Sender<EmulationEvent>,
        handle: ClientHandle,
    ) {
        log::debug!("deactivating client {handle}");
        match self.client_manager.borrow_mut().get_mut(handle) {
            Some((_, s)) => s.active = false,
            None => return,
        };

        let _ = capture.send(CaptureEvent::Destroy(handle)).await;
        let _ = emulate.send(EmulationEvent::Destroy(handle)).await;
        log::debug!("deactivating client {handle} done");
    }

    async fn activate_client(
        &self,
        capture: &Sender<CaptureEvent>,
        emulate: &Sender<EmulationEvent>,
        handle: ClientHandle,
    ) {
        log::debug!("activating client");
        /* deactivate potential other client at this position */
        let pos = match self.client_manager.borrow().get(handle) {
            Some((client, _)) => client.pos,
            None => return,
        };

        let other = self.client_manager.borrow_mut().find_client(pos);
        if let Some(other) = other {
            if other != handle {
                self.deactivate_client(capture, emulate, other).await;
            }
        }

        /* activate the client */
        if let Some((_, s)) = self.client_manager.borrow_mut().get_mut(handle) {
            s.active = true;
        } else {
            return;
        };

        /* notify emulation, capture and frontends */
        let _ = capture.send(CaptureEvent::Create(handle, pos.into())).await;
        let _ = emulate.send(EmulationEvent::Create(handle)).await;
        log::debug!("activating client {handle} done");
    }

    async fn remove_client(
        &self,
        capture: &Sender<CaptureEvent>,
        emulate: &Sender<EmulationEvent>,
        handle: ClientHandle,
    ) {
        let Some(active) = self
            .client_manager
            .borrow_mut()
            .remove_client(handle)
            .map(|(_, s)| s.active)
        else {
            return;
        };

        if active {
            let _ = capture.send(CaptureEvent::Destroy(handle)).await;
            let _ = emulate.send(EmulationEvent::Destroy(handle)).await;
        }
    }

    fn update_fix_ips(&self, handle: ClientHandle, fix_ips: Vec<IpAddr>) {
        let mut client_manager = self.client_manager.borrow_mut();
        let Some((c, _)) = client_manager.get_mut(handle) else {
            return;
        };

        c.fix_ips = fix_ips;
    }

    fn update_hostname(&self, handle: ClientHandle, hostname: Option<String>) {
        let mut client_manager = self.client_manager.borrow_mut();
        let Some((c, s)) = client_manager.get_mut(handle) else {
            return;
        };

        // hostname changed
        if c.hostname != hostname {
            c.hostname = hostname;
            s.ips = HashSet::from_iter(c.fix_ips.iter().cloned());
            s.active_addr = None;
            self.request_dns(handle);
        }
    }

    fn update_port(&self, handle: ClientHandle, port: u16) {
        let mut client_manager = self.client_manager.borrow_mut();
        let Some((c, s)) = client_manager.get_mut(handle) else {
            return;
        };

        if c.port != port {
            c.port = port;
            s.active_addr = s.active_addr.map(|a| SocketAddr::new(a.ip(), port));
        }
    }

    async fn update_pos(
        &self,
        handle: ClientHandle,
        capture: &Sender<CaptureEvent>,
        emulate: &Sender<EmulationEvent>,
        pos: Position,
    ) {
        let (changed, active) = {
            let mut client_manager = self.client_manager.borrow_mut();
            let Some((c, s)) = client_manager.get_mut(handle) else {
                return;
            };

            let changed = c.pos != pos;
            c.pos = pos;
            (changed, s.active)
        };

        // update state in event input emulator & input capture
        if changed {
            if active {
                let _ = capture.send(CaptureEvent::Destroy(handle)).await;
                let _ = emulate.send(EmulationEvent::Destroy(handle)).await;
            }
            let _ = capture.send(CaptureEvent::Create(handle, pos.into())).await;
            let _ = emulate.send(EmulationEvent::Create(handle)).await;
        }
    }

    fn broadcast_client(&self, handle: ClientHandle) {
        let client = self.client_manager.borrow().get(handle).cloned();
        let event = if let Some((config, state)) = client {
            FrontendEvent::State(handle, config, state)
        } else {
            FrontendEvent::NoSuchClient(handle)
        };
        self.notify_frontend(event);
    }

    fn set_emulation_status(&self, status: Status) {
        self.emulation_status.replace(status);
        let status = FrontendEvent::EmulationStatus(status);
        self.notify_frontend(status);
    }

    fn set_capture_status(&self, status: Status) {
        self.capture_status.replace(status);
        let status = FrontendEvent::CaptureStatus(status);
        self.notify_frontend(status);
    }
}

async fn listen_frontend(
    request_tx: Sender<FrontendRequest>,
    #[cfg(unix)] mut stream: ReadHalf<UnixStream>,
    #[cfg(windows)] mut stream: ReadHalf<TcpStream>,
) {
    use std::io;
    loop {
        let request = frontend::wait_for_request(&mut stream).await;
        match request {
            Ok(request) => {
                let _ = request_tx.send(request).await;
            }
            Err(e) => {
                if let Some(e) = e.downcast_ref::<io::Error>() {
                    if e.kind() == ErrorKind::UnexpectedEof {
                        return;
                    }
                }
                log::error!("error reading frontend event: {e}");
                return;
            }
        }
    }
}

fn handle_frontend_stream(
    cancel: CancellationToken,
    #[cfg(unix)] stream: ReadHalf<UnixStream>,
    #[cfg(windows)] stream: ReadHalf<TcpStream>,
    request_tx: Sender<FrontendRequest>,
) -> JoinHandle<()> {
    tokio::task::spawn_local(async move {
        tokio::select! {
            _ = listen_frontend(request_tx, stream) => {},
            _ = cancel.cancelled() => {},
        }
    })
}
