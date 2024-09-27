use crate::{
    capture::Capture,
    client::ClientManager,
    config::Config,
    connect::LanMouseConnection,
    dns::DnsResolver,
    emulation::Emulation,
    listen::{LanMouseListener, ListenerCreationError},
};
use futures::StreamExt;
use hickory_resolver::error::ResolveError;
use lan_mouse_ipc::{
    AsyncFrontendListener, ClientConfig, ClientHandle, ClientState, FrontendEvent, FrontendRequest,
    IpcListenerCreationError, Position, Status,
};
use log;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    io,
    net::{IpAddr, SocketAddr},
    rc::Rc,
};
use thiserror::Error;
use tokio::{signal, sync::Notify};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error(transparent)]
    Dns(#[from] ResolveError),
    #[error(transparent)]
    IpcListen(#[from] IpcListenerCreationError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    ListenError(#[from] ListenerCreationError),
}

pub struct ReleaseToken;

#[derive(Clone)]
pub struct Server {
    active: Rc<Cell<Option<ClientHandle>>>,
    authorized_keys: Rc<RefCell<HashSet<String>>>,
    known_hosts: Rc<RefCell<HashSet<String>>>,
    pub(crate) client_manager: ClientManager,
    port: Rc<Cell<u16>>,
    notifies: Rc<Notifies>,
    pub(crate) config: Rc<Config>,
    pending_frontend_events: Rc<RefCell<VecDeque<FrontendEvent>>>,
    capture_status: Rc<Cell<Status>>,
    pub(crate) emulation_status: Rc<Cell<Status>>,
    pub(crate) should_release: Rc<RefCell<Option<ReleaseToken>>>,
    incoming_conns: Rc<RefCell<HashMap<SocketAddr, Position>>>,
}

#[derive(Default)]
struct Notifies {
    capture: Notify,
    emulation: Notify,
    port_changed: Notify,
    frontend_event_pending: Notify,
    cancel: CancellationToken,
}

impl Server {
    pub fn new(config: Config) -> Self {
        let client_manager = ClientManager::default();
        let port = Rc::new(Cell::new(config.port));
        for client in config.get_clients() {
            let config = ClientConfig {
                hostname: client.hostname,
                fix_ips: client.ips.into_iter().collect(),
                port: client.port,
                pos: client.pos,
                cmd: client.enter_hook,
            };
            let state = ClientState {
                active: client.active,
                ips: HashSet::from_iter(config.fix_ips.iter().cloned()),
                ..Default::default()
            };
            let handle = client_manager.add_client();
            client_manager.set_config(handle, config);
            client_manager.set_state(handle, state);
        }

        // task notification tokens
        let notifies = Rc::new(Notifies::default());

        let config = Rc::new(config);

        Self {
            active: Rc::new(Cell::new(None)),
            authorized_keys: Default::default(),
            known_hosts: Default::default(),
            config,
            client_manager,
            port,
            notifies,
            pending_frontend_events: Rc::new(RefCell::new(VecDeque::new())),
            capture_status: Default::default(),
            emulation_status: Default::default(),
            incoming_conns: Rc::new(RefCell::new(HashMap::new())),
            should_release: Default::default(),
        }
    }

    pub async fn run(&mut self) -> Result<(), ServiceError> {
        // create frontend communication adapter, exit if already running
        let mut frontend = match AsyncFrontendListener::new().await {
            Ok(f) => f,
            Err(IpcListenerCreationError::AlreadyRunning) => {
                log::info!("service already running, exiting");
                return Ok(());
            }
            e => e?,
        };

        // listener + connection
        let listener = LanMouseListener::new(self.config.port).await?;
        let conn = LanMouseConnection::new(self.clone());

        // input capture + emulation
        let mut capture = Capture::new(self.clone(), conn);
        let mut emulation = Emulation::new(self.clone(), listener);

        // create dns resolver
        let resolver = DnsResolver::new(self.clone())?;

        for handle in self.client_manager.active_clients() {
            resolver.resolve(handle);
        }

        loop {
            tokio::select! {
                request = frontend.next() => {
                    let request = match request {
                        Some(Ok(r)) => r,
                        Some(Err(e)) => {
                            log::error!("error receiving request: {e}");
                            continue;
                        }
                        None => break,
                    };
                    log::debug!("received frontend request: {request:?}");
                    self.handle_request(&capture, request, &resolver);
                    log::debug!("handled frontend request");
                }
                _ = self.notifies.frontend_event_pending.notified() => {
                    while let Some(event) = {
                        /* need to drop borrow before next iteration! */
                        let event = self.pending_frontend_events.borrow_mut().pop_front();
                        event
                    } {
                        frontend.broadcast(event).await;
                    }
                },
                _ = self.cancelled() => break,
                r = signal::ctrl_c() => {
                    r.expect("failed to wait for CTRL+C");
                    break;
                }
            }
        }

        log::info!("terminating service");

        self.cancel();

        capture.terminate().await;
        emulation.terminate().await;

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

    fn notify_capture(&self) {
        log::info!("received capture enable request");
        self.notifies.capture.notify_waiters()
    }

    pub(crate) async fn capture_enabled(&self) {
        self.notifies.capture.notified().await
    }

    fn notify_emulation(&self) {
        log::info!("received emulation enable request");
        self.notifies.emulation.notify_waiters()
    }

    pub(crate) async fn emulation_notified(&self) {
        self.notifies.emulation.notified().await
    }

    fn request_port_change(&self, port: u16) {
        self.port.replace(port);
        self.notifies.port_changed.notify_one();
    }

    #[allow(unused)]
    fn notify_port_changed(&self, port: u16, msg: Option<String>) {
        self.port.replace(port);
        self.notify_frontend(FrontendEvent::PortChanged(port, msg));
    }

    pub(crate) fn client_updated(&self, handle: ClientHandle) {
        self.notify_frontend(FrontendEvent::Changed(handle));
    }

    fn handle_request(&self, capture: &Capture, event: FrontendRequest, dns: &DnsResolver) -> bool {
        log::debug!("frontend: {event:?}");
        match event {
            FrontendRequest::EnableCapture => self.notify_capture(),
            FrontendRequest::EnableEmulation => self.notify_emulation(),
            FrontendRequest::Create => {
                self.add_client();
            }
            FrontendRequest::Activate(handle, active) => {
                if active {
                    self.activate_client(capture, handle);
                } else {
                    self.deactivate_client(capture, handle);
                }
            }
            FrontendRequest::ChangePort(port) => self.request_port_change(port),
            FrontendRequest::Delete(handle) => {
                self.remove_client(capture, handle);
                self.notify_frontend(FrontendEvent::Deleted(handle));
            }
            FrontendRequest::Enumerate() => self.enumerate(),
            FrontendRequest::GetState(handle) => self.broadcast_client(handle),
            FrontendRequest::UpdateFixIps(handle, fix_ips) => self.update_fix_ips(handle, fix_ips),
            FrontendRequest::UpdateHostname(handle, host) => {
                self.update_hostname(handle, host, dns)
            }
            FrontendRequest::UpdatePort(handle, port) => self.update_port(handle, port),
            FrontendRequest::UpdatePosition(handle, pos) => {
                self.update_pos(handle, capture, pos);
            }
            FrontendRequest::ResolveDns(handle) => dns.resolve(handle),
            FrontendRequest::Sync => {
                self.enumerate();
                self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status.get()));
                self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status.get()));
                self.notify_frontend(FrontendEvent::PortChanged(self.port.get(), None));
            }
            FrontendRequest::FingerprintAdd(key) => {
                self.add_authorized_key(key);
            }
            FrontendRequest::FingerprintRemove(key) => {
                self.remove_authorized_key(key);
            }
        };
        false
    }

    fn add_authorized_key(&self, key: String) {
        self.authorized_keys.borrow_mut().insert(key);
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(
            self.authorized_keys.borrow().clone(),
        ));
    }

    fn remove_authorized_key(&self, key: String) {
        self.authorized_keys.borrow_mut().remove(&key);
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(
            self.authorized_keys.borrow().clone(),
        ));
    }

    fn enumerate(&self) {
        let clients = self.client_manager.get_client_states();
        self.notify_frontend(FrontendEvent::Enumerate(clients));
    }

    fn add_client(&self) -> ClientHandle {
        let handle = self.client_manager.add_client();
        log::info!("added client {handle}");
        let (c, s) = self.client_manager.get_state(handle).unwrap();
        self.notify_frontend(FrontendEvent::Created(handle, c, s));
        handle
    }

    fn deactivate_client(&self, capture: &Capture, handle: ClientHandle) {
        log::debug!("deactivating client {handle}");
        if self.client_manager.deactivate_client(handle) {
            capture.destroy(handle);
            self.client_updated(handle);
            log::info!("deactivated client {handle}");
        }
    }

    fn activate_client(&self, capture: &Capture, handle: ClientHandle) {
        log::debug!("activating client");
        /* deactivate potential other client at this position */
        let Some(pos) = self.client_manager.get_pos(handle) else {
            return;
        };

        if let Some(other) = self.client_manager.client_at(pos) {
            if other != handle {
                self.deactivate_client(capture, other);
            }
        }

        /* activate the client */
        if self.client_manager.activate_client(handle) {
            /* notify capture and frontends */
            capture.create(handle, pos);
            self.client_updated(handle);
            log::info!("activated client {handle} ({pos})");
        }
    }

    fn remove_client(&self, capture: &Capture, handle: ClientHandle) {
        if let Some(true) = self
            .client_manager
            .remove_client(handle)
            .map(|(_, s)| s.active)
        {
            capture.destroy(handle);
        }
    }

    fn update_fix_ips(&self, handle: ClientHandle, fix_ips: Vec<IpAddr>) {
        self.client_manager.set_fix_ips(handle, fix_ips);
        self.client_updated(handle);
    }

    pub(crate) fn update_dns_ips(&self, handle: ClientHandle, dns_ips: Vec<IpAddr>) {
        self.client_manager.set_dns_ips(handle, dns_ips);
        self.client_updated(handle);
    }

    fn update_hostname(&self, handle: ClientHandle, hostname: Option<String>, dns: &DnsResolver) {
        if self.client_manager.set_hostname(handle, hostname) {
            dns.resolve(handle);
            self.client_updated(handle);
        }
    }

    fn update_port(&self, handle: ClientHandle, port: u16) {
        self.client_manager.set_port(handle, port);
    }

    fn update_pos(&self, handle: ClientHandle, capture: &Capture, pos: Position) {
        // update state in event input emulator & input capture
        if self.client_manager.set_pos(handle, pos) {
            self.deactivate_client(capture, handle);
            self.activate_client(capture, handle);
        }
    }

    fn broadcast_client(&self, handle: ClientHandle) {
        let event = if let Some((config, state)) = self.client_manager.get_state(handle) {
            FrontendEvent::State(handle, config, state)
        } else {
            FrontendEvent::NoSuchClient(handle)
        };
        self.notify_frontend(event);
    }

    pub(crate) fn set_emulation_status(&self, status: Status) {
        self.emulation_status.replace(status);
        let status = FrontendEvent::EmulationStatus(status);
        self.notify_frontend(status);
    }

    pub(crate) fn set_capture_status(&self, status: Status) {
        self.capture_status.replace(status);
        let status = FrontendEvent::CaptureStatus(status);
        self.notify_frontend(status);
    }

    pub(crate) fn set_resolving(&self, handle: ClientHandle, status: bool) {
        self.client_manager.set_resolving(handle, status);
        self.client_updated(handle);
    }

    pub(crate) fn release_capture(&self) {
        self.should_release.replace(Some(ReleaseToken));
    }

    pub(crate) fn set_active(&self, handle: Option<ClientHandle>) {
        self.active.replace(handle);
    }

    pub(crate) fn get_active(&self) -> Option<ClientHandle> {
        self.active.get()
    }

    pub(crate) fn register_incoming(&self, addr: SocketAddr, pos: Position) {
        self.incoming_conns.borrow_mut().insert(addr, pos);
    }
}
