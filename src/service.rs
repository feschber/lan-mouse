use crate::{
    capture::{Capture, ICaptureEvent},
    client::ClientManager,
    config::Config,
    connect::LanMouseConnection,
    crypto,
    dns::{DnsEvent, DnsResolver},
    emulation::{Emulation, EmulationEvent},
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
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio::{signal, sync::Notify};
use webrtc_dtls::crypto::Certificate;

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
    #[error("failed to load certificate: `{0}`")]
    Certificate(#[from] crypto::Error),
}

pub struct ReleaseToken;

enum IncomingEvent {
    Connected {
        addr: SocketAddr,
        pos: Position,
        fingerprint: String,
    },
    Disconnected {
        addr: SocketAddr,
    },
}

#[derive(Debug)]
pub struct Incoming {
    fingerprint: String,
    addr: SocketAddr,
    pos: Position,
}

#[derive(Clone)]
pub struct Service {
    authorized_keys: Arc<RwLock<HashMap<String, String>>>,
    pub(crate) client_manager: ClientManager,
    port: Rc<Cell<u16>>,
    public_key_fingerprint: String,
    notifies: Rc<Notifies>,
    pub(crate) config: Rc<Config>,
    pending_frontend_events: Rc<RefCell<VecDeque<FrontendEvent>>>,
    pending_incoming: Rc<RefCell<VecDeque<IncomingEvent>>>,
    capture_status: Rc<Cell<Status>>,
    pub(crate) emulation_status: Rc<Cell<Status>>,
    /// keep track of registered connections to avoid duplicate barriers
    incoming_conns: Rc<RefCell<HashSet<SocketAddr>>>,
    /// map from capture handle to connection info
    incoming_conn_info: Rc<RefCell<HashMap<ClientHandle, Incoming>>>,
    cert: Certificate,
    next_trigger_handle: u64,
}

#[derive(Default)]
struct Notifies {
    incoming: Notify,
    frontend_event_pending: Notify,
}

impl Service {
    pub async fn new(config: Config) -> Result<Self, ServiceError> {
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

        // load certificate
        let cert = crypto::load_or_generate_key_and_cert(&config.cert_path)?;
        let public_key_fingerprint = crypto::certificate_fingerprint(&cert);

        let service = Self {
            authorized_keys: Arc::new(RwLock::new(config.authorized_fingerprints.clone())),
            cert,
            public_key_fingerprint,
            config: Rc::new(config),
            client_manager,
            pending_incoming: Default::default(),
            port,
            notifies: Default::default(),
            pending_frontend_events: Rc::new(RefCell::new(VecDeque::new())),
            capture_status: Default::default(),
            emulation_status: Default::default(),
            incoming_conn_info: Default::default(),
            incoming_conns: Default::default(),
            next_trigger_handle: 0,
        };
        Ok(service)
    }

    pub async fn run(&mut self) -> Result<(), ServiceError> {
        // create frontend communication adapter, exit if already running
        let mut frontend_listener = AsyncFrontendListener::new().await?;

        // listener + connection
        let listener = LanMouseListener::new(
            self.config.port,
            self.cert.clone(),
            self.authorized_keys.clone(),
        )
        .await?;
        let conn = LanMouseConnection::new(self.clone(), self.cert.clone());

        // input capture + emulation
        let capture_backend = self.config.capture_backend.map(|b| b.into());
        let mut capture = Capture::new(capture_backend, conn, self.clone());
        let emulation_backend = self.config.emulation_backend.map(|b| b.into());
        let mut emulation = Emulation::new(emulation_backend, listener);

        // create dns resolver
        let mut resolver = DnsResolver::new()?;

        for handle in self.client_manager.active_clients() {
            if let Some(hostname) = self.client_manager.get_hostname(handle) {
                resolver.resolve(handle, hostname);
            }
        }

        loop {
            tokio::select! {
                request = frontend_listener.next() => {
                    let request = match request {
                        Some(Ok(r)) => r,
                        Some(Err(e)) => {
                            log::error!("error receiving request: {e}");
                            continue;
                        }
                        None => break,
                    };
                    match request {
                        FrontendRequest::EnableCapture => capture.reenable(),
                        FrontendRequest::EnableEmulation => emulation.reenable(),
                        FrontendRequest::Create => {
                            self.add_client();
                        }
                        FrontendRequest::Activate(handle, active) => {
                            if active {
                                if let Some(hostname) = self.client_manager.get_hostname(handle) {
                                    resolver.resolve(handle, hostname);
                                }
                                self.activate_client(&capture, handle);
                            } else {
                                self.deactivate_client(&capture, handle);
                            }
                        }
                        FrontendRequest::ChangePort(port) => emulation.request_port_change(port),
                        FrontendRequest::Delete(handle) => {
                            self.remove_client(&capture, handle);
                            self.notify_frontend(FrontendEvent::Deleted(handle));
                        }
                        FrontendRequest::Enumerate() => self.enumerate(),
                        FrontendRequest::GetState(handle) => self.broadcast_client(handle),
                        FrontendRequest::UpdateFixIps(handle, fix_ips) => self.update_fix_ips(handle, fix_ips),
                        FrontendRequest::UpdateHostname(handle, host) => {
                            self.update_hostname(handle, host, &resolver)
                        }
                        FrontendRequest::UpdatePort(handle, port) => self.update_port(handle, port),
                        FrontendRequest::UpdatePosition(handle, pos) => {
                            self.update_pos(handle, &capture, pos);
                        }
                        FrontendRequest::ResolveDns(handle) => {
                            if let Some(hostname) = self.client_manager.get_hostname(handle) {
                                resolver.resolve(handle, hostname);
                            }
                        }
                        FrontendRequest::Sync => {
                            self.enumerate();
                            self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status.get()));
                            self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status.get()));
                            self.notify_frontend(FrontendEvent::PortChanged(self.port.get(), None));
                            self.notify_frontend(FrontendEvent::PublicKeyFingerprint(
                                self.public_key_fingerprint.clone(),
                            ));
                            self.notify_frontend(FrontendEvent::AuthorizedUpdated(
                                self.authorized_keys.read().expect("lock").clone(),
                            ));
                        }
                        FrontendRequest::AuthorizeKey(desc, fp) => {
                            self.add_authorized_key(desc, fp);
                        }
                        FrontendRequest::RemoveAuthorizedKey(key) => {
                            self.remove_authorized_key(key);
                        }
                    }
                }
                _ = self.notifies.frontend_event_pending.notified() => {
                    while let Some(event) = {
                        /* need to drop borrow before next iteration! */
                        let event = self.pending_frontend_events.borrow_mut().pop_front();
                        event
                    } {
                        frontend_listener.broadcast(event).await;
                    }
                },
                _ = self.notifies.incoming.notified() => {
                    while let Some(incoming) = {
                        let incoming = self.pending_incoming.borrow_mut().pop_front();
                        incoming
                    } {
                        match incoming {
                            IncomingEvent::Connected { addr, pos, fingerprint } => {
                                // check if already registered
                                if self.incoming_conns.borrow_mut().insert(addr) {
                                    self.add_incoming(addr, pos, fingerprint.clone(), &capture);
                                    self.notify_frontend(FrontendEvent::IncomingConnected(fingerprint, addr, pos));
                                }
                            },
                            IncomingEvent::Disconnected { addr } => {
                                if let Some(fp) = self.remove_incoming(addr, &capture) {
                                    self.notify_frontend(FrontendEvent::IncomingDisconnected(fp));
                                }
                            },
                        }
                    }
                },
                event = emulation.event() => match event {
                    EmulationEvent::Connected { addr, pos, fingerprint } => self.register_incoming(addr, pos, fingerprint),
                    EmulationEvent::Disconnected { addr } => self.deregister_incoming(addr),
                    EmulationEvent::PortChanged(port) => match port {
                        Ok(port) => {
                            self.port.replace(port);
                            self.notify_frontend(FrontendEvent::PortChanged(port, None));
                        },
                        Err(e) => self.notify_frontend(FrontendEvent::PortChanged(self.port.get(), Some(format!("{e}")))),
                    }
                    EmulationEvent::EmulationDisabled => {
                        self.emulation_status.replace(Status::Disabled);
                        self.notify_frontend(FrontendEvent::EmulationStatus(Status::Disabled));
                    },
                    EmulationEvent::EmulationEnabled => {
                        self.emulation_status.replace(Status::Enabled);
                        self.notify_frontend(FrontendEvent::EmulationStatus(Status::Enabled));
                    },
                    EmulationEvent::ReleaseNotify => capture.release(),
                },
                event = capture.event() => match event {
                    ICaptureEvent::ClientEntered(handle) => {
                        // we entered the capture zone for an incoming connection
                        // => notify it that its capture should be released
                        if let Some(incoming) = self.incoming_conn_info.borrow().get(&handle) {
                            emulation.send_leave_event(incoming.addr);
                        }
                    }
                    ICaptureEvent::CaptureDisabled => {
                        self.capture_status.replace(Status::Disabled);
                        self.notify_frontend(FrontendEvent::CaptureStatus(Status::Disabled));
                    }
                    ICaptureEvent::CaptureEnabled => {
                        self.capture_status.replace(Status::Enabled);
                        self.notify_frontend(FrontendEvent::CaptureStatus(Status::Enabled));
                    }
                },
                event = resolver.event() => match event {
                    DnsEvent::Resolving(handle) => self.set_resolving(handle, true),
                    DnsEvent::Resolved(handle, hostname, ips) => {
                        self.set_resolving(handle, false);
                        match ips {
                            Ok(ips) => self.update_dns_ips(handle, ips),
                            Err(e) => {
                                log::warn!("could not resolve {hostname}: {e}");
                                self.update_dns_ips(handle, vec![]);
                            },
                        }
                    }
                },
                r = signal::ctrl_c() => {
                    r.expect("failed to wait for CTRL+C");
                    break;
                }
            }
        }

        log::info!("terminating service ...");
        log::info!("terminating capture ...");
        capture.terminate().await;
        log::info!("terminating emulation ...");
        emulation.terminate().await;
        log::info!("terminating dns resolver ...");
        resolver.terminate().await;

        Ok(())
    }

    pub(crate) const ENTER_HANDLE_BEGIN: u64 = u64::MAX / 2 + 1;

    fn add_incoming(
        &mut self,
        addr: SocketAddr,
        pos: Position,
        fingerprint: String,
        capture: &Capture,
    ) {
        let handle = Self::ENTER_HANDLE_BEGIN + self.next_trigger_handle;
        self.next_trigger_handle += 1;
        capture.create(handle, pos);
        self.incoming_conn_info.borrow_mut().insert(
            handle,
            Incoming {
                fingerprint,
                addr,
                pos,
            },
        );
    }

    fn remove_incoming(&mut self, addr: SocketAddr, capture: &Capture) -> Option<String> {
        let handle = self
            .incoming_conn_info
            .borrow()
            .iter()
            .find(|(_, incoming)| incoming.addr == addr)
            .map(|(k, _)| *k)?;
        capture.destroy(handle);
        self.incoming_conns.borrow_mut().remove(&addr);
        self.incoming_conn_info
            .borrow_mut()
            .remove(&handle)
            .map(|incoming| incoming.fingerprint)
    }

    pub(crate) fn get_incoming_pos(&self, handle: ClientHandle) -> Option<Position> {
        self.incoming_conn_info
            .borrow()
            .get(&handle)
            .map(|incoming| incoming.pos)
    }

    fn notify_frontend(&self, event: FrontendEvent) {
        self.pending_frontend_events.borrow_mut().push_back(event);
        self.notifies.frontend_event_pending.notify_one();
    }

    pub(crate) fn client_updated(&self, handle: ClientHandle) {
        self.notify_frontend(FrontendEvent::Changed(handle));
    }

    fn add_authorized_key(&self, desc: String, fp: String) {
        self.authorized_keys.write().expect("lock").insert(fp, desc);
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(
            self.authorized_keys.read().expect("lock").clone(),
        ));
    }

    fn remove_authorized_key(&self, fp: String) {
        self.authorized_keys.write().expect("lock").remove(&fp);
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(
            self.authorized_keys.read().expect("lock").clone(),
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
        if self.client_manager.set_hostname(handle, hostname.clone()) {
            if let Some(hostname) = hostname {
                dns.resolve(handle, hostname);
            }
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

    pub(crate) fn set_resolving(&self, handle: ClientHandle, status: bool) {
        self.client_manager.set_resolving(handle, status);
        self.client_updated(handle);
    }

    pub(crate) fn register_incoming(&self, addr: SocketAddr, pos: Position, fingerprint: String) {
        self.pending_incoming
            .borrow_mut()
            .push_back(IncomingEvent::Connected {
                addr,
                pos,
                fingerprint,
            });
        self.notifies.incoming.notify_one();
    }

    pub(crate) fn deregister_incoming(&self, addr: SocketAddr) {
        self.pending_incoming
            .borrow_mut()
            .push_back(IncomingEvent::Disconnected { addr });
    }
}
