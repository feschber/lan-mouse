use log;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use tokio::signal;

use crate::{capture, emulate};
use crate::{
    client::{ClientHandle, ClientManager},
    config::Config,
    dns,
    frontend::{FrontendListener, FrontendRequest},
    server::capture_task::CaptureEvent,
};

use self::{emulation_task::EmulationEvent, resolver_task::DnsRequest};

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
    release_bind: Vec<crate::scancode::Linux>,
}

impl Server {
    pub fn new(config: &Config) -> Self {
        let active_client = Rc::new(Cell::new(None));
        let client_manager = Rc::new(RefCell::new(ClientManager::new()));
        let state = Rc::new(Cell::new(State::Receiving));
        let port = Rc::new(Cell::new(config.port));
        for config_client in config.get_clients() {
            client_manager.borrow_mut().add_client(
                config_client.hostname,
                config_client.ips,
                config_client.port,
                config_client.pos,
                config_client.active,
            );
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

    pub async fn run(&self) -> anyhow::Result<()> {
        // create frontend communication adapter
        let frontend = match FrontendListener::new().await {
            Some(f) => f?,
            None => {
                // none means some other instance is already running
                log::info!("service already running, exiting");
                return anyhow::Ok(());
            }
        };
        let (emulate, capture) = tokio::join!(emulate::create(), capture::create());

        let (timer_tx, timer_rx) = tokio::sync::mpsc::channel(1);
        let (frontend_notify_tx, frontend_notify_rx) = tokio::sync::mpsc::channel(1);

        // udp task
        let (mut udp_task, sender_tx, receiver_rx, port_tx) =
            network_task::new(self.clone(), frontend_notify_tx).await?;

        // input capture
        let (mut capture_task, capture_channel) = capture_task::new(
            capture,
            self.clone(),
            sender_tx.clone(),
            timer_tx.clone(),
            self.release_bind.clone(),
        );

        // input emulation
        let (mut emulation_task, emulate_channel) = emulation_task::new(
            emulate,
            self.clone(),
            receiver_rx,
            sender_tx.clone(),
            capture_channel.clone(),
            timer_tx,
        );

        // create dns resolver
        let resolver = dns::DnsResolver::new().await?;
        let (mut resolver_task, resolve_tx) = resolver_task::new(resolver, self.clone());

        // frontend listener
        let (mut frontend_task, frontend_tx) = frontend_task::new(
            frontend,
            frontend_notify_rx,
            self.clone(),
            capture_channel.clone(),
            emulate_channel.clone(),
            resolve_tx.clone(),
            port_tx,
        );

        // task that pings clients to see if they are responding
        let mut ping_task = ping_task::new(
            self.clone(),
            sender_tx.clone(),
            emulate_channel.clone(),
            capture_channel.clone(),
            timer_rx,
        );

        let active = self
            .client_manager
            .borrow()
            .get_client_states()
            .filter_map(|(h, s)| {
                if s.active {
                    Some((h, s.client.hostname.clone()))
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
                let _ = resolve_tx.send(DnsRequest { hostname, handle }).await;
            }
        }
        log::info!("running service");

        tokio::select! {
            _ = signal::ctrl_c() => {
                log::info!("terminating service");
            }
            e = &mut capture_task => {
                if let Ok(Err(e)) = e {
                    log::error!("error in input capture task: {e}");
                }
            }
            e = &mut emulation_task => {
                if let Ok(Err(e)) = e {
                    log::error!("error in input emulation task: {e}");
                }
            }
            e = &mut frontend_task => {
                if let Ok(Err(e)) = e {
                    log::error!("error in frontend listener: {e}");
                }
            }
            _ = &mut resolver_task => { }
            _ = &mut udp_task => { }
            _ = &mut ping_task => { }
        }

        let _ = emulate_channel.send(EmulationEvent::Terminate).await;
        let _ = capture_channel.send(CaptureEvent::Terminate).await;
        let _ = frontend_tx.send(FrontendRequest::Terminate()).await;

        if !capture_task.is_finished() {
            if let Err(e) = capture_task.await {
                log::error!("error in input capture task: {e}");
            }
        }
        if !emulation_task.is_finished() {
            if let Err(e) = emulation_task.await {
                log::error!("error in input emulation task: {e}");
            }
        }

        if !frontend_task.is_finished() {
            if let Err(e) = frontend_task.await {
                log::error!("error in frontend listener: {e}");
            }
        }

        resolver_task.abort();
        udp_task.abort();
        ping_task.abort();

        Ok(())
    }
}
