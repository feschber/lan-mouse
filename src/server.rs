use log;
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
};
use tokio::{
    join, signal,
    sync::{mpsc::channel, Notify},
};
use tokio_util::sync::CancellationToken;

use crate::{
    client::{ClientConfig, ClientHandle, ClientManager, ClientState},
    config::{CaptureBackend, Config, EmulationBackend},
    dns::DnsResolver,
    frontend::{FrontendListener, FrontendRequest},
    server::capture_task::CaptureEvent,
};

use self::resolver_task::DnsRequest;

mod capture_task;
mod emulation_task;
mod frontend_task;
mod network_task;
mod ping_task;
mod resolver_task;

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
    client_manager: Rc<RefCell<ClientManager>>,
    port: Rc<Cell<u16>>,
    state: Rc<Cell<State>>,
    release_bind: Vec<input_event::scancode::Linux>,
    notifies: Rc<Notifies>,
}

#[derive(Default)]
struct Notifies {
    ping: Notify,
    capture: Notify,
    emulation: Notify,
    cancel: CancellationToken,
}

impl Server {
    pub fn new(config: &Config) -> Self {
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

        Self {
            active_client,
            client_manager,
            port,
            state,
            release_bind,
            notifies,
        }
    }

    pub async fn run(
        &self,
        capture_backend: Option<CaptureBackend>,
        emulation_backend: Option<EmulationBackend>,
    ) -> anyhow::Result<()> {
        // create frontend communication adapter, exit if already running
        let frontend = match FrontendListener::new().await {
            Some(f) => f?,
            None => {
                // none means some other instance is already running
                log::info!("service already running, exiting");
                return anyhow::Ok(());
            }
        };

        let (frontend_tx, frontend_rx) = channel(1); /* events for frontends */
        let (request_tx, request_rx) = channel(1); /* requests coming from frontends */
        let (capture_tx, capture_rx) = channel(1); /* requests for input capture */
        let (emulation_tx, emulation_rx) = channel(1); /* emulation requests */
        let (udp_recv_tx, udp_recv_rx) = channel(1); /* udp receiver */
        let (udp_send_tx, udp_send_rx) = channel(1); /* udp sender */
        let (port_tx, port_rx) = channel(1); /* port change request */
        let (dns_tx, dns_rx) = channel(1); /* dns requests */

        // udp task
        let network = network_task::new(
            self.clone(),
            udp_recv_tx,
            udp_send_rx,
            port_rx,
            frontend_tx.clone(),
        )
        .await?;

        // input capture
        let capture = capture_task::new(
            self.clone(),
            capture_backend,
            capture_rx,
            udp_send_tx.clone(),
            frontend_tx.clone(),
            self.release_bind.clone(),
        );

        // input emulation
        let emulation = emulation_task::new(
            self.clone(),
            emulation_backend,
            emulation_rx,
            udp_recv_rx,
            udp_send_tx.clone(),
            capture_tx.clone(),
            frontend_tx.clone(),
        );

        // create dns resolver
        let resolver = DnsResolver::new().await?;
        let resolver = resolver_task::new(resolver, dns_rx, self.clone(), frontend_tx);

        // frontend listener
        let frontend = frontend_task::new(
            self.clone(),
            frontend,
            frontend_rx,
            request_tx.clone(),
            request_rx,
            capture_tx.clone(),
            emulation_tx.clone(),
            dns_tx.clone(),
            port_tx,
        );

        // task that pings clients to see if they are responding
        let ping = ping_task::new(
            self.clone(),
            udp_send_tx.clone(),
            emulation_tx.clone(),
            capture_tx.clone(),
        );

        let active = self
            .client_manager
            .borrow()
            .get_client_states()
            .filter_map(|(h, (c, s))| {
                if s.active {
                    Some((h, c.hostname.clone()))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for (handle, hostname) in active {
            request_tx
                .send(FrontendRequest::Activate(handle, true))
                .await?;
            if let Some(hostname) = hostname {
                let _ = dns_tx.send(DnsRequest { hostname, handle }).await;
            }
        }

        log::info!("running service");
        signal::ctrl_c().await.expect("failed to listen for CTRL+C");
        log::info!("terminating service");

        self.cancel();
        let _ = join!(capture, emulation, frontend, network, resolver, ping);

        Ok(())
    }

    fn cancel(&self) {
        self.notifies.cancel.cancel();
    }

    async fn cancelled(&self) {
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
}
