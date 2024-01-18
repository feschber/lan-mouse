use log;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use tokio::signal;

use crate::{
    client::{ClientHandle, ClientManager},
    config::Config,
    dns,
    frontend::{FrontendEvent, FrontendListener},
    server::producer_task::ProducerEvent,
};
use crate::{consumer, producer};

use self::{consumer_task::ConsumerEvent, resolver_task::DnsRequest};

mod consumer_task;
mod frontend_task;
mod network_task;
mod ping_task;
mod producer_task;
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
        Self {
            active_client,
            client_manager,
            port,
            state,
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
        let (consumer, producer) = tokio::join!(consumer::create(), producer::create());

        let (timer_tx, timer_rx) = tokio::sync::mpsc::channel(1);
        let (frontend_notify_tx, frontend_notify_rx) = tokio::sync::mpsc::channel(1);

        // udp task
        let (mut udp_task, sender_tx, receiver_rx, port_tx) =
            network_task::new(self.clone(), frontend_notify_tx).await?;

        // event producer
        let (mut producer_task, producer_channel) =
            producer_task::new(producer, self.clone(), sender_tx.clone(), timer_tx.clone());

        // event consumer
        let (mut consumer_task, consumer_channel) = consumer_task::new(
            consumer,
            self.clone(),
            receiver_rx,
            sender_tx.clone(),
            producer_channel.clone(),
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
            producer_channel.clone(),
            consumer_channel.clone(),
            resolve_tx.clone(),
            port_tx,
        );

        // task that pings clients to see if they are responding
        let mut ping_task = ping_task::new(
            self.clone(),
            sender_tx.clone(),
            consumer_channel.clone(),
            producer_channel.clone(),
            timer_rx,
        );

        let active = self
            .client_manager
            .borrow()
            .get_client_states()
            .filter_map(|s| {
                if s.active {
                    Some((s.client.handle, s.client.hostname.clone()))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for (handle, hostname) in active {
            frontend_tx
                .send(FrontendEvent::ActivateClient(handle, true))
                .await?;
            if let Some(hostname) = hostname {
                let _ = resolve_tx.send(DnsRequest { hostname, handle }).await;
            }
        }

        tokio::select! {
            _ = signal::ctrl_c() => {
                log::info!("terminating service");
            }
            e = &mut producer_task => {
                if let Ok(Err(e)) = e {
                    log::error!("error in event producer: {e}");
                }
            }
            e = &mut consumer_task => {
                if let Ok(Err(e)) = e {
                    log::error!("error in event consumer: {e}");
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

        let _ = consumer_channel.send(ConsumerEvent::Terminate).await;
        let _ = producer_channel.send(ProducerEvent::Terminate).await;
        let _ = frontend_tx.send(FrontendEvent::Shutdown()).await;

        if !producer_task.is_finished() {
            if let Err(e) = producer_task.await {
                log::error!("error in event producer: {e}");
            }
        }
        if !consumer_task.is_finished() {
            if let Err(e) = consumer_task.await {
                log::error!("error in event consumer: {e}");
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
