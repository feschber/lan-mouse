use crate::{
    capture::{Capture, CaptureType, ICaptureEvent},
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
    collections::{HashMap, HashSet, VecDeque},
    io,
    net::{IpAddr, SocketAddr},
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio::{process::Command, signal, sync::Notify};

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

pub struct Service {
    capture: Capture,
    emulation: Emulation,
    resolver: DnsResolver,
    frontend_listener: AsyncFrontendListener,
    authorized_keys: Arc<RwLock<HashMap<String, String>>>,
    client_manager: ClientManager,
    port: u16,
    public_key_fingerprint: String,
    frontend_event_pending: Notify,
    pending_frontend_events: VecDeque<FrontendEvent>,
    capture_status: Status,
    emulation_status: Status,
    /// keep track of registered connections to avoid duplicate barriers
    incoming_conns: HashSet<SocketAddr>,
    /// map from capture handle to connection info
    incoming_conn_info: HashMap<ClientHandle, Incoming>,
    next_trigger_handle: u64,
}

#[derive(Debug)]
struct Incoming {
    fingerprint: String,
    addr: SocketAddr,
    pos: Position,
}

impl Service {
    pub async fn new(config: Config) -> Result<Self, ServiceError> {
        let client_manager = ClientManager::default();
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

        // create frontend communication adapter, exit if already running
        let frontend_listener = AsyncFrontendListener::new().await?;

        let authorized_keys = Arc::new(RwLock::new(config.authorized_fingerprints.clone()));
        // listener + connection
        let listener =
            LanMouseListener::new(config.port, cert.clone(), authorized_keys.clone()).await?;
        let conn = LanMouseConnection::new(cert.clone(), client_manager.clone());

        // input capture + emulation
        let capture_backend = config.capture_backend.map(|b| b.into());
        let capture = Capture::new(capture_backend, conn, config.release_bind.clone());
        let emulation_backend = config.emulation_backend.map(|b| b.into());
        let emulation = Emulation::new(emulation_backend, listener);

        // create dns resolver
        let resolver = DnsResolver::new()?;

        let port = config.port;
        let service = Self {
            capture,
            emulation,
            frontend_listener,
            resolver,
            authorized_keys,
            public_key_fingerprint,
            client_manager,
            frontend_event_pending: Default::default(),
            port,
            pending_frontend_events: Default::default(),
            capture_status: Default::default(),
            emulation_status: Default::default(),
            incoming_conn_info: Default::default(),
            incoming_conns: Default::default(),
            next_trigger_handle: 0,
        };
        Ok(service)
    }

    pub async fn run(&mut self) -> Result<(), ServiceError> {
        for handle in self.client_manager.active_clients() {
            if let Some(hostname) = self.client_manager.get_hostname(handle) {
                self.resolver.resolve(handle, hostname);
            }
            if let Some(pos) = self.client_manager.get_pos(handle) {
                self.capture.create(handle, pos, CaptureType::Default);
            }
        }

        loop {
            tokio::select! {
                request = self.frontend_listener.next() => {
                    let request = match request {
                        Some(Ok(r)) => r,
                        Some(Err(e)) => {
                            log::error!("error receiving request: {e}");
                            continue;
                        }
                        None => break,
                    };
                    match request {
                        FrontendRequest::EnableCapture => self.capture.reenable(),
                        FrontendRequest::EnableEmulation => self.emulation.reenable(),
                        FrontendRequest::Create => {
                            self.add_client();
                        }
                        FrontendRequest::Activate(handle, active) => {
                            if active {
                                if let Some(hostname) = self.client_manager.get_hostname(handle) {
                                    self.resolver.resolve(handle, hostname);
                                }
                                self.activate_client(handle);
                            } else {
                                self.deactivate_client(handle);
                            }
                        }
                        FrontendRequest::ChangePort(port) => {
                            if self.port != port {
                                self.emulation.request_port_change(port);
                            } else {
                                self.notify_frontend(FrontendEvent::PortChanged(self.port, None));
                            }
                        }
                        FrontendRequest::Delete(handle) => {
                            self.remove_client(handle);
                            self.notify_frontend(FrontendEvent::Deleted(handle));
                        }
                        FrontendRequest::Enumerate() => self.enumerate(),
                        FrontendRequest::GetState(handle) => self.broadcast_client(handle),
                        FrontendRequest::UpdateFixIps(handle, fix_ips) => self.update_fix_ips(handle, fix_ips),
                        FrontendRequest::UpdateHostname(handle, host) => {
                            self.update_hostname(handle, host)
                        }
                        FrontendRequest::UpdatePort(handle, port) => self.update_port(handle, port),
                        FrontendRequest::UpdatePosition(handle, pos) => {
                            self.update_pos(handle, pos);
                        }
                        FrontendRequest::ResolveDns(handle) => {
                            if let Some(hostname) = self.client_manager.get_hostname(handle) {
                                self.resolver.resolve(handle, hostname);
                            }
                        }
                        FrontendRequest::Sync => {
                            self.enumerate();
                            self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
                            self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
                            self.notify_frontend(FrontendEvent::PortChanged(self.port, None));
                            self.notify_frontend(FrontendEvent::PublicKeyFingerprint(
                                self.public_key_fingerprint.clone(),
                            ));
                            let keys =  self.authorized_keys.read().expect("lock").clone();
                            self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
                        }
                        FrontendRequest::AuthorizeKey(desc, fp) => {
                            self.add_authorized_key(desc, fp);
                        }
                        FrontendRequest::RemoveAuthorizedKey(key) => {
                            self.remove_authorized_key(key);
                        }
                    }
                }
                _ = self.frontend_event_pending.notified() => {
                    while let Some(event) = self.pending_frontend_events.pop_front() {
                        self.frontend_listener.broadcast(event).await;
                    }
                },
                event = self.emulation.event() => match event {
                    EmulationEvent::Connected { addr, pos, fingerprint } => {
                        // check if already registered
                        if !self.incoming_conns.contains(&addr) {
                            self.add_incoming(addr, pos, fingerprint.clone());
                            self.notify_frontend(FrontendEvent::IncomingConnected(fingerprint, addr, pos));
                        } else {
                            let handle = self
                                .incoming_conn_info
                                .iter()
                                .find(|(_, incoming)| incoming.addr == addr)
                                .map(|(k, _)| *k)
                                .expect("no such client");
                            let mut changed = false;
                            if let Some(incoming) = self.incoming_conn_info.get_mut(&handle) {
                                if incoming.fingerprint != fingerprint {
                                    incoming.fingerprint = fingerprint.clone();
                                    changed = true;
                                }
                                if incoming.pos != pos {
                                    incoming.pos = pos;
                                    changed = true;
                                }
                            }
                            if changed {
                                self.remove_incoming(addr);
                                self.add_incoming(addr, pos, fingerprint.clone());
                                self.notify_frontend(FrontendEvent::IncomingDisconnected(addr));
                                self.notify_frontend(FrontendEvent::IncomingConnected(fingerprint, addr, pos));
                            }
                        }
                    }
                    EmulationEvent::Disconnected { addr } => {
                        if let Some(addr) = self.remove_incoming(addr) {
                            self.notify_frontend(FrontendEvent::IncomingDisconnected(addr));
                        }
                    }
                    EmulationEvent::PortChanged(port) => match port {
                        Ok(port) => {
                            self.port = port;
                            self.notify_frontend(FrontendEvent::PortChanged(port, None));
                        },
                        Err(e) => self.notify_frontend(FrontendEvent::PortChanged(self.port, Some(format!("{e}")))),
                    }
                    EmulationEvent::EmulationDisabled => {
                        self.emulation_status = Status::Disabled;
                        self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
                    },
                    EmulationEvent::EmulationEnabled => {
                        self.emulation_status = Status::Enabled;
                        self.notify_frontend(FrontendEvent::EmulationStatus(self.emulation_status));
                    },
                    EmulationEvent::ReleaseNotify => self.capture.release(),
                },
                event = self.capture.event() => match event {
                    ICaptureEvent::CaptureBegin(handle) => {
                        // we entered the capture zone for an incoming connection
                        // => notify it that its capture should be released
                        if let Some(incoming) = self.incoming_conn_info.get(&handle) {
                            self.emulation.send_leave_event(incoming.addr);
                        }
                    }
                    ICaptureEvent::CaptureDisabled => {
                        self.capture_status = Status::Disabled;
                        self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
                    }
                    ICaptureEvent::CaptureEnabled => {
                        self.capture_status = Status::Enabled;
                        self.notify_frontend(FrontendEvent::CaptureStatus(self.capture_status));
                    }
                    ICaptureEvent::ClientEntered(handle) => {
                        log::info!("entering client {handle} ...");
                        self.spawn_hook_command(handle);
                    },
                },
                event = self.resolver.event() => match event {
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
        self.capture.terminate().await;
        log::info!("terminating emulation ...");
        self.emulation.terminate().await;
        log::info!("terminating dns resolver ...");
        self.resolver.terminate().await;

        Ok(())
    }

    pub(crate) const ENTER_HANDLE_BEGIN: u64 = u64::MAX / 2 + 1;

    fn add_incoming(&mut self, addr: SocketAddr, pos: Position, fingerprint: String) {
        let handle = Self::ENTER_HANDLE_BEGIN + self.next_trigger_handle;
        self.next_trigger_handle += 1;
        self.capture.create(handle, pos, CaptureType::EnterOnly);
        self.incoming_conns.insert(addr);
        self.incoming_conn_info.insert(
            handle,
            Incoming {
                fingerprint,
                addr,
                pos,
            },
        );
    }

    fn remove_incoming(&mut self, addr: SocketAddr) -> Option<SocketAddr> {
        let handle = self
            .incoming_conn_info
            .iter()
            .find(|(_, incoming)| incoming.addr == addr)
            .map(|(k, _)| *k)?;
        self.capture.destroy(handle);
        self.incoming_conns.remove(&addr);
        self.incoming_conn_info
            .remove(&handle)
            .map(|incoming| incoming.addr)
    }

    fn notify_frontend(&mut self, event: FrontendEvent) {
        self.pending_frontend_events.push_back(event);
        self.frontend_event_pending.notify_one();
    }

    fn client_updated(&mut self, handle: ClientHandle) {
        self.notify_frontend(FrontendEvent::Changed(handle));
    }

    fn add_authorized_key(&mut self, desc: String, fp: String) {
        self.authorized_keys.write().expect("lock").insert(fp, desc);
        let keys = self.authorized_keys.read().expect("lock").clone();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
    }

    fn remove_authorized_key(&mut self, fp: String) {
        self.authorized_keys.write().expect("lock").remove(&fp);
        let keys = self.authorized_keys.read().expect("lock").clone();
        self.notify_frontend(FrontendEvent::AuthorizedUpdated(keys));
    }

    fn enumerate(&mut self) {
        let clients = self.client_manager.get_client_states();
        self.notify_frontend(FrontendEvent::Enumerate(clients));
    }

    fn add_client(&mut self) -> ClientHandle {
        let handle = self.client_manager.add_client();
        log::info!("added client {handle}");
        let (c, s) = self.client_manager.get_state(handle).unwrap();
        self.notify_frontend(FrontendEvent::Created(handle, c, s));
        handle
    }

    fn deactivate_client(&mut self, handle: ClientHandle) {
        log::debug!("deactivating client {handle}");
        if self.client_manager.deactivate_client(handle) {
            self.capture.destroy(handle);
            self.client_updated(handle);
            log::info!("deactivated client {handle}");
        }
    }

    fn activate_client(&mut self, handle: ClientHandle) {
        log::debug!("activating client");
        /* deactivate potential other client at this position */
        let Some(pos) = self.client_manager.get_pos(handle) else {
            return;
        };

        if let Some(other) = self.client_manager.client_at(pos) {
            if other != handle {
                self.deactivate_client(other);
            }
        }

        /* activate the client */
        if self.client_manager.activate_client(handle) {
            /* notify capture and frontends */
            self.capture.create(handle, pos, CaptureType::Default);
            self.client_updated(handle);
            log::info!("activated client {handle} ({pos})");
        }
    }

    fn remove_client(&self, handle: ClientHandle) {
        if let Some(true) = self
            .client_manager
            .remove_client(handle)
            .map(|(_, s)| s.active)
        {
            self.capture.destroy(handle);
        }
    }

    fn update_fix_ips(&mut self, handle: ClientHandle, fix_ips: Vec<IpAddr>) {
        self.client_manager.set_fix_ips(handle, fix_ips);
        self.client_updated(handle);
    }

    fn update_dns_ips(&mut self, handle: ClientHandle, dns_ips: Vec<IpAddr>) {
        self.client_manager.set_dns_ips(handle, dns_ips);
        self.client_updated(handle);
    }

    fn update_hostname(&mut self, handle: ClientHandle, hostname: Option<String>) {
        if self.client_manager.set_hostname(handle, hostname.clone()) {
            if let Some(hostname) = hostname {
                self.resolver.resolve(handle, hostname);
            }
            self.client_updated(handle);
        }
    }

    fn update_port(&self, handle: ClientHandle, port: u16) {
        self.client_manager.set_port(handle, port);
    }

    fn update_pos(&mut self, handle: ClientHandle, pos: Position) {
        // update state in event input emulator & input capture
        if self.client_manager.set_pos(handle, pos) {
            self.deactivate_client(handle);
            self.activate_client(handle);
        }
    }

    fn broadcast_client(&mut self, handle: ClientHandle) {
        let event = if let Some((config, state)) = self.client_manager.get_state(handle) {
            FrontendEvent::State(handle, config, state)
        } else {
            FrontendEvent::NoSuchClient(handle)
        };
        self.notify_frontend(event);
    }

    fn set_resolving(&mut self, handle: ClientHandle, status: bool) {
        self.client_manager.set_resolving(handle, status);
        self.client_updated(handle);
    }

    fn spawn_hook_command(&self, handle: ClientHandle) {
        let Some(cmd) = self.client_manager.get_enter_cmd(handle) else {
            return;
        };
        tokio::task::spawn_local(async move {
            log::info!("spawning command!");
            let mut child = match Command::new("sh").arg("-c").arg(cmd.as_str()).spawn() {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("could not execute cmd: {e}");
                    return;
                }
            };
            match child.wait().await {
                Ok(s) => {
                    if s.success() {
                        log::info!("{cmd} exited successfully");
                    } else {
                        log::warn!("{cmd} exited with {s}");
                    }
                }
                Err(e) => log::warn!("{cmd}: {e}"),
            }
        });
    }
}
