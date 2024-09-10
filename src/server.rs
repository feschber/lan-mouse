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
    collections::{HashSet, VecDeque},
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
    pub(crate) client_manager: Rc<RefCell<ClientManager>>,
    port: Rc<Cell<u16>>,
    pub(crate) release_bind: Vec<input_event::scancode::Linux>,
    notifies: Rc<Notifies>,
    pub(crate) config: Rc<Config>,
    pending_frontend_events: Rc<RefCell<VecDeque<FrontendEvent>>>,
    capture_status: Rc<Cell<Status>>,
    pub(crate) emulation_status: Rc<Cell<Status>>,
    pub(crate) should_release: Rc<RefCell<Option<ReleaseToken>>>,
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
        let client_manager = Rc::new(RefCell::new(ClientManager::default()));
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
            client_manager,
            port,
            release_bind,
            notifies,
            pending_frontend_events: Rc::new(RefCell::new(VecDeque::new())),
            capture_status: Default::default(),
            emulation_status: Default::default(),
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

        for handle in self.active_clients() {
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

    pub(crate) fn active_clients(&self) -> Vec<ClientHandle> {
        self.client_manager
            .borrow()
            .get_client_states()
            .filter(|(_, (_, s))| s.active)
            .map(|(h, _)| h)
            .collect()
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

    fn add_client(&self) -> ClientHandle {
        let handle = self.client_manager.borrow_mut().add_client();
        log::info!("added client {handle}");
        let (c, s) = self.client_manager.borrow().get(handle).unwrap().clone();
        self.notify_frontend(FrontendEvent::Created(handle, c, s));
        handle
    }

    fn deactivate_client(&self, capture: &Capture, handle: ClientHandle) {
        log::debug!("deactivating client {handle}");
        match self.client_manager.borrow_mut().get_mut(handle) {
            None => return,
            Some((_, s)) if !s.active => return,
            Some((_, s)) => s.active = false,
        };

        capture.destroy(handle);
        self.client_updated(handle);
        log::info!("deactivated client {handle}");
    }

    fn activate_client(&self, capture: &Capture, handle: ClientHandle) {
        log::debug!("activating client");
        /* deactivate potential other client at this position */
        let pos = match self.client_manager.borrow().get(handle) {
            None => return,
            Some((_, s)) if s.active => return,
            Some((client, _)) => client.pos,
        };

        let other = self.client_manager.borrow_mut().find_client(pos);
        if let Some(other) = other {
            self.deactivate_client(capture, other);
        }

        /* activate the client */
        if let Some((_, s)) = self.client_manager.borrow_mut().get_mut(handle) {
            s.active = true;
        } else {
            return;
        };

        /* notify capture and frontends */
        capture.create(handle, to_capture_pos(pos));
        self.client_updated(handle);
        log::info!("activated client {handle} ({pos})");
    }

    fn remove_client(&self, capture: &Capture, handle: ClientHandle) {
        let Some(active) = self
            .client_manager
            .borrow_mut()
            .remove_client(handle)
            .map(|(_, s)| s.active)
        else {
            return;
        };

        if active {
            capture.destroy(handle);
        }
    }

    fn update_fix_ips(&self, handle: ClientHandle, fix_ips: Vec<IpAddr>) {
        if let Some((c, _)) = self.client_manager.borrow_mut().get_mut(handle) {
            c.fix_ips = fix_ips;
        };
        self.update_ips(handle);
        self.client_updated(handle);
    }

    pub(crate) fn update_dns_ips(&self, handle: ClientHandle, dns_ips: Vec<IpAddr>) {
        if let Some((_, s)) = self.client_manager.borrow_mut().get_mut(handle) {
            s.dns_ips = dns_ips;
        };
        self.update_ips(handle);
        self.client_updated(handle);
    }

    fn update_ips(&self, handle: ClientHandle) {
        if let Some((c, s)) = self.client_manager.borrow_mut().get_mut(handle) {
            s.ips = c
                .fix_ips
                .iter()
                .cloned()
                .chain(s.dns_ips.iter().cloned())
                .collect::<HashSet<_>>();
        }
    }

    pub(crate) fn get_ips(&self, handle: ClientHandle) -> Option<Vec<IpAddr>> {
        if let Some((_, s)) = self.client_manager.borrow().get(handle) {
            Some(s.ips.iter().copied().collect())
        } else {
            None
        }
    }

    pub(crate) fn get_port(&self, handle: ClientHandle) -> Option<u16> {
        if let Some((c, _)) = self.client_manager.borrow().get(handle) {
            Some(c.port)
        } else {
            None
        }
    }

    fn update_hostname(&self, handle: ClientHandle, hostname: Option<String>, dns: &DnsResolver) {
        let mut client_manager = self.client_manager.borrow_mut();
        let Some((c, s)) = client_manager.get_mut(handle) else {
            return;
        };

        // hostname changed
        if c.hostname != hostname {
            c.hostname = hostname;
            s.active_addr = None;
            s.dns_ips.clear();
            drop(client_manager);
            self.update_ips(handle);
            dns.resolve(handle);
        }
        self.client_updated(handle);
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

    fn update_pos(&self, handle: ClientHandle, capture: &Capture, pos: Position) {
        let (changed, active) = {
            let mut client_manager = self.client_manager.borrow_mut();
            let Some((c, s)) = client_manager.get_mut(handle) else {
                return;
            };

            let changed = c.pos != pos;
            if changed {
                log::info!("update pos {handle} {} -> {}", c.pos, pos);
            }
            c.pos = pos;
            (changed, s.active)
        };

        // update state in event input emulator & input capture
        if changed {
            self.deactivate_client(capture, handle);
            if active {
                self.activate_client(capture, handle);
            }
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
        if let Some((_, s)) = self.client_manager.borrow_mut().get_mut(handle) {
            s.resolving = status;
        }
        self.client_updated(handle);
    }

    pub(crate) fn get_hostname(&self, handle: ClientHandle) -> Option<String> {
        self.client_manager
            .borrow_mut()
            .get_mut(handle)
            .and_then(|(c, _)| c.hostname.clone())
    }

    pub(crate) fn get_pos(&self, handle: ClientHandle) -> Option<Position> {
        self.client_manager.borrow().get(handle).map(|(c, _)| c.pos)
    }

    pub(crate) fn set_active_addr(&self, handle: ClientHandle, addr: Option<SocketAddr>) {
        if let Some((_, s)) = self.client_manager.borrow_mut().get_mut(handle) {
            s.active_addr = addr;
        }
    }

    pub(crate) fn active_addr(&self, handle: ClientHandle) -> Option<SocketAddr> {
        self.client_manager
            .borrow()
            .get(handle)
            .and_then(|(_, s)| s.active_addr)
    }

    pub(crate) fn release_capture(&self) {
        self.should_release.replace(Some(ReleaseToken));
    }
}

fn to_capture_pos(pos: Position) -> input_capture::Position {
    match pos {
        Position::Left => input_capture::Position::Left,
        Position::Right => input_capture::Position::Right,
        Position::Top => input_capture::Position::Top,
        Position::Bottom => input_capture::Position::Bottom,
    }
}
