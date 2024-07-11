use log;
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    rc::Rc,
    sync::Arc,
};
use tokio::{
    join, signal,
    sync::{mpsc::channel, Notify},
};
use tokio_util::sync::CancellationToken;

use crate::{
    client::{ClientConfig, ClientHandle, ClientManager, ClientState},
    config::{CaptureBackend, Config, EmulationBackend},
    dns,
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
}

impl Server {
    pub fn new(config: &Config) -> Self {
        let active_client = Rc::new(Cell::new(None));
        let client_manager = Rc::new(RefCell::new(ClientManager::new()));
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
        let release_bind = config.release_bind.clone();
        Self {
            active_client,
            client_manager,
            port,
            state,
            release_bind,
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

        let notify_ping = Arc::new(Notify::new()); /* notify ping timer restart */
        let (frontend_tx, frontend_rx) = channel(1); /* events coming from frontends */
        let cancellation_token = CancellationToken::new(); /* notify termination */
        let notify_capture = Arc::new(Notify::new()); /* notify capture restart */
        let notify_emulation = Arc::new(Notify::new()); /* notify emultation restart */

        // udp task
        let (mut network, udp_send, udp_recv, port_tx) = network_task::new(
            self.clone(),
            frontend_tx.clone(),
            cancellation_token.clone(),
        )
        .await?;

        // input capture
        let (mut capture, capture_channel) = capture_task::new(
            capture_backend,
            self.clone(),
            udp_send.clone(),
            frontend_tx.clone(),
            notify_ping.clone(),
            self.release_bind.clone(),
            cancellation_token.clone(),
            notify_capture.clone(),
        );

        // input emulation
        let (mut emulation, emulate_channel) = emulation_task::new(
            emulation_backend,
            self.clone(),
            udp_recv,
            udp_send.clone(),
            capture_channel.clone(),
            frontend_tx.clone(),
            notify_ping.clone(),
            cancellation_token.clone(),
            notify_emulation.clone(),
        );

        // create dns resolver
        let resolver = dns::DnsResolver::new().await?;
        let (mut resolver, dns_req) = resolver_task::new(
            resolver,
            self.clone(),
            frontend_tx,
            cancellation_token.clone(),
        );

        // frontend listener
        let (mut frontend, frontend_tx) = frontend_task::new(
            frontend,
            frontend_rx,
            self.clone(),
            notify_emulation,
            notify_capture,
            capture_channel.clone(),
            emulate_channel.clone(),
            dns_req.clone(),
            port_tx,
            cancellation_token.clone(),
        );

        // task that pings clients to see if they are responding
        let mut ping = ping_task::new(
            self.clone(),
            udp_send.clone(),
            emulate_channel.clone(),
            capture_channel.clone(),
            notify_ping,
            cancellation_token.clone(),
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
            frontend_tx
                .send(FrontendRequest::Activate(handle, true))
                .await?;
            if let Some(hostname) = hostname {
                let _ = dns_req.send(DnsRequest { hostname, handle }).await;
            }
        }
        log::info!("running service");

        tokio::select! {
            _ = signal::ctrl_c() => log::info!("terminating service"),
            _ = &mut capture => { }
            _ = &mut emulation => { }
            _ = &mut frontend => { }
            _ = &mut resolver => { }
            _ = &mut network => { }
            _ = &mut ping => { }
        }

        cancellation_token.cancel();
        let _ = join!(capture, emulation, frontend, network, resolver, ping);

        Ok(())
    }
}
