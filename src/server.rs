use anyhow::anyhow;
use futures::stream::StreamExt;
use log;
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    error::Error,
    io::Result,
    net::IpAddr,
    rc::Rc,
    time::{Duration, Instant},
};
use tokio::{io::ReadHalf, net::UdpSocket, signal, sync::mpsc::Sender};

#[cfg(unix)]
use tokio::net::UnixStream;

#[cfg(windows)]
use tokio::net::TcpStream;

use std::{io::ErrorKind, net::SocketAddr};

use crate::{
    client::{ClientEvent, ClientHandle, ClientManager, Position},
    config::Config,
    consumer::EventConsumer,
    dns,
    frontend::{self, FrontendEvent, FrontendListener, FrontendNotify},
    producer::EventProducer,
};
use crate::{
    consumer,
    event::{Event, KeyboardEvent},
    producer,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum State {
    Sending,
    Receiving,
    AwaitingLeave,
}

pub enum ProducerEvent {
    Release,
    ClientEvent(ClientEvent),
}

pub enum ConsumerEvent {
    ClientEvent(ClientEvent),
    PortChange(u16),
}

pub struct Server {}

impl Server {
    pub async fn run(config: &Config) -> anyhow::Result<()> {
        // create frontend communication adapter
        let mut frontend = match FrontendListener::new().await {
            Some(Err(e)) => return Err(e),
            Some(Ok(f)) => f,
            None => {
                // none means some other instance is already running
                log::info!("service already running, exiting");
                return anyhow::Ok(());
            }
        };
        let (mut consumer, mut producer) = tokio::join!(consumer::create(), producer::create());

        // create dns resolver
        let resolver = dns::DnsResolver::new().await?;

        // bind the udp socket
        let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), config.port);
        let socket_rc = Rc::new(RefCell::new(UdpSocket::bind(listen_addr).await?));
        let (frontend_tx, mut frontend_rx) = tokio::sync::mpsc::channel(1);

        // create client manager
        let client_manager_rc = Rc::new(RefCell::new(ClientManager::new()));

        let state_rc = Rc::new(Cell::new(State::Receiving));

        // channel to notify producer
        let (producer_notify_tx, mut producer_notify_rx) = tokio::sync::mpsc::channel(32);

        // channel to notify consumer
        let (consumer_notify_tx, mut consumer_notify_rx) = tokio::sync::mpsc::channel(32);

        // channel to request dns resolver
        let (resolve_tx, mut resolve_rx) = tokio::sync::mpsc::channel(32);

        // channel to send events to frontends
        let (frontend_notify_tx, mut frontend_notify_rx) = tokio::sync::mpsc::channel(32);

        // add clients from config
        for (c, h, port, p) in config.get_clients().into_iter() {
            Self::add_client(
                &resolve_tx,
                &client_manager_rc,
                &mut frontend,
                h,
                c,
                port,
                p,
            )
            .await;
        }

        // event producer
        let client_manager = client_manager_rc.clone();
        let state = state_rc.clone();
        let socket = socket_rc.clone();
        let producer_task = tokio::task::spawn_local(async move {
            loop {
                tokio::select! {
                    e = producer.next() => {
                        let (client, event) = match e {
                            Some(e) => e?,
                            None => return Err::<(), anyhow::Error>(anyhow!("event producer closed")),
                        };
                        Self::handle_producer_event(&mut producer, &client_manager, &state, &socket, client, event);
                    }
                    e = producer_notify_rx.recv() => {
                        match e {
                            Some(e) => match e {
                                ProducerEvent::Release => producer.release(),
                                ProducerEvent::ClientEvent(e) => producer.notify(e),
                            },
                            None => break Ok(()),
                        }
                    }
                }
            }
        });

        // event consumer
        let client_manager = client_manager_rc.clone();
        let socket = socket_rc.clone();
        let state = state_rc.clone();
        let producer_notify = producer_notify_tx.clone();
        let receiver_task = tokio::task::spawn_local(async move {
            let mut last_ignored = None;

            loop {
                tokio::select! {
                    udp_event = receive_event(&socket) => {
                        let udp_event = match udp_event {
                            Ok(e) => e,
                            Err(e) => return Err::<(), anyhow::Error>(anyhow!("{}", e)),
                        };
                        Self::handle_udp_rx(&client_manager, &producer_notify, &mut consumer, &socket, &state, &mut last_ignored, udp_event).await;
                    }
                    consumer_event = consumer_notify_rx.recv() => {
                        match consumer_event {
                            Some(e) => match e {
                                ConsumerEvent::ClientEvent(e) => consumer.notify(e).await,
                                ConsumerEvent::PortChange(port) => {
                                    let listen_addr = SocketAddr::new("0.0.0.0".parse().unwrap(), port);
                                    match UdpSocket::bind(listen_addr).await {
                                        Ok(new_socket) => {
                                            socket.replace(new_socket);
                                            let _ = frontend_notify_tx.send(FrontendNotify::NotifyPortChange(port, None)).await;
                                        }
                                        Err(e) => {
                                            log::warn!("could not change port: {e}");
                                            let port = socket.borrow().local_addr().unwrap().port();
                                            let _ = frontend_notify_tx.send(FrontendNotify::NotifyPortChange(
                                                    port,
                                                    Some(format!("could not change port: {e}")),
                                                )).await;
                                        }
                                    }
                                }
                            },
                            None => break,
                        }
                    }
                    _ = consumer.dispatch() => { }
                }
            }
            // destroy consumer
            consumer.destroy().await;
            Ok(())
        });

        // frontend listener
        let socket = socket_rc.clone();
        let client_manager = client_manager_rc.clone();
        let frontend_task = tokio::task::spawn_local(async move {
            loop {
                tokio::select! {
                    stream = frontend.accept() => {
                        let stream = match stream {
                            Ok(s) => s,
                            Err(e) => {
                                log::warn!("error accepting frontend connection: {e}");
                                continue;
                            }
                        };
                        Self::handle_frontend_stream(&client_manager, &mut frontend, &frontend_tx, stream).await;
                    }
                    event = frontend_rx.recv() => {
                        let frontend_event = match event {
                            Some(e) => e,
                            None => return Err::<(), anyhow::Error>(anyhow!("frontend channel closed")),
                        };
                        Self::handle_frontend_event(&producer_notify_tx, &consumer_notify_tx, &client_manager, &resolve_tx, &mut frontend, &socket, frontend_event).await;
                    }
                    notify = frontend_notify_rx.recv() => {
                        let notify = match notify {
                            Some(n) => n,
                            None => return Err::<(), anyhow::Error>(anyhow!("frontend notify closed")),
                        };
                        let _ = frontend.notify_all(notify).await;
                    }
                }
            }
        });

        let client_manager = client_manager_rc.clone();
        let resolver_task = tokio::task::spawn_local(async move {
            loop {
                let (host, client): (String, ClientHandle) = match resolve_rx.recv().await {
                    Some(r) => r,
                    None => break,
                };
                let ips = match resolver.resolve(&host).await {
                    Ok(ips) => ips,
                    Err(e) => {
                        log::warn!("could not resolve host '{host}': {e}");
                        continue;
                    }
                };
                if let Some(state) = client_manager.borrow_mut().get_mut(client) {
                    let port = state.client.port;
                    let mut addrs = HashSet::from_iter(
                        state
                            .client
                            .fix_ips
                            .iter()
                            .map(|a| SocketAddr::new(*a, port)),
                    );
                    for ip in ips {
                        let sock_addr = SocketAddr::new(ip, port);
                        addrs.insert(sock_addr);
                    }
                    state.client.addrs = addrs;
                }
            }
        });

        _ = signal::ctrl_c().await;

        producer_task.await??;
        receiver_task.await??;
        frontend_task.await??;
        resolver_task.await?;

        Ok(())
    }

    pub async fn add_client(
        resolver_tx: &Sender<(String, ClientHandle)>,
        client_manager: &Rc<RefCell<ClientManager>>,
        frontend: &mut FrontendListener,
        hostname: Option<String>,
        addr: HashSet<IpAddr>,
        port: u16,
        pos: Position,
    ) -> ClientHandle {
        log::info!(
            "adding client [{}]{} @ {:?}",
            pos,
            hostname.as_deref().unwrap_or(""),
            &addr
        );
        let client = client_manager
            .borrow_mut()
            .add_client(hostname.clone(), addr, port, pos);

        log::debug!("add_client {client}");
        if let Some(hostname) = hostname.clone() {
            let _ = resolver_tx.send((hostname, client)).await;
        };
        let notify = FrontendNotify::NotifyClientCreate(client, hostname, port, pos);
        if let Err(e) = frontend.notify_all(notify).await {
            log::error!("error notifying frontend: {e}");
        };
        client
    }

    pub async fn activate_client(
        producer_notify_tx: &Sender<ProducerEvent>,
        consumer_notify_tx: &Sender<ConsumerEvent>,
        client_manager: &Rc<RefCell<ClientManager>>,
        client: ClientHandle,
        active: bool,
    ) {
        let (client, pos) = match client_manager.borrow_mut().get_mut(client) {
            Some(state) => {
                state.active = active;
                (state.client.handle, state.client.pos)
            }
            None => return,
        };
        if active {
            let _ = producer_notify_tx
                .send(ProducerEvent::ClientEvent(ClientEvent::Create(client, pos)))
                .await;
            let _ = consumer_notify_tx
                .send(ConsumerEvent::ClientEvent(ClientEvent::Create(client, pos)))
                .await;
        } else {
            let _ = producer_notify_tx
                .send(ProducerEvent::ClientEvent(ClientEvent::Destroy(client)))
                .await;
            let _ = consumer_notify_tx
                .send(ConsumerEvent::ClientEvent(ClientEvent::Destroy(client)))
                .await;
        }
    }

    pub async fn remove_client(
        client_manager: &Rc<RefCell<ClientManager>>,
        producer_notify_tx: &Sender<ProducerEvent>,
        consumer_notify_tx: &Sender<ConsumerEvent>,
        frontend: &mut FrontendListener,
        client: ClientHandle,
    ) -> Option<ClientHandle> {
        let _ = producer_notify_tx
            .send(ProducerEvent::ClientEvent(ClientEvent::Destroy(client)))
            .await;
        let _ = consumer_notify_tx
            .send(ConsumerEvent::ClientEvent(ClientEvent::Destroy(client)))
            .await;
        if let Some(client) = client_manager
            .borrow_mut()
            .remove_client(client)
            .map(|s| s.client.handle)
        {
            let notify = FrontendNotify::NotifyClientDelete(client);
            log::debug!("{notify:?}");
            if let Err(e) = frontend.notify_all(notify).await {
                log::error!("error notifying frontend: {e}");
            }
            Some(client)
        } else {
            None
        }
    }

    pub async fn update_client(
        producer_notify_tx: &Sender<ProducerEvent>,
        consumer_notify_tx: &Sender<ConsumerEvent>,
        resolve_tx: &Sender<(String, ClientHandle)>,
        client_manager: &Rc<RefCell<ClientManager>>,
        client: ClientHandle,
        hostname: Option<String>,
        port: u16,
        pos: Position,
    ) {
        // retrieve state
        let mut client_manager = client_manager.borrow_mut();
        let Some(state) = client_manager.get_mut(client) else {
            return;
        };

        // update pos
        state.client.pos = pos;
        if state.active {
            let _ = producer_notify_tx
                .send(ProducerEvent::ClientEvent(ClientEvent::Destroy(client)))
                .await;
            let _ = consumer_notify_tx
                .send(ConsumerEvent::ClientEvent(ClientEvent::Destroy(client)))
                .await;
            let _ = producer_notify_tx
                .send(ProducerEvent::ClientEvent(ClientEvent::Create(client, pos)))
                .await;
            let _ = consumer_notify_tx
                .send(ConsumerEvent::ClientEvent(ClientEvent::Create(client, pos)))
                .await;
        }

        // update port
        if state.client.port != port {
            state.client.port = port;
            state.client.addrs = state
                .client
                .addrs
                .iter()
                .cloned()
                .map(|mut a| {
                    a.set_port(port);
                    a
                })
                .collect();
            state
                .client
                .active_addr
                .map(|a| SocketAddr::new(a.ip(), port));
        }

        // update hostname
        if state.client.hostname != hostname {
            state.client.addrs = HashSet::new();
            state.client.active_addr = None;
            state.client.hostname = hostname;
            if let Some(hostname) = state.client.hostname.clone() {
                let _ = resolve_tx.send((hostname, state.client.handle)).await;
            }
        }
        log::debug!("client updated: {:?}", state);
    }

    async fn handle_udp_rx(
        client_manager: &Rc<RefCell<ClientManager>>,
        producer_notify_tx: &Sender<ProducerEvent>,
        consumer: &mut Box<dyn EventConsumer>,
        socket: &Rc<RefCell<UdpSocket>>,
        state: &Rc<Cell<State>>,
        last_ignored: &mut Option<SocketAddr>,
        event: (Event, SocketAddr),
    ) {
        let (event, addr) = event;

        // get handle for addr
        let handle = match client_manager.borrow().get_client(addr) {
            Some(a) => a,
            None => {
                if last_ignored.is_none() || last_ignored.is_some() && last_ignored.unwrap() != addr
                {
                    log::warn!("ignoring events from client {addr}");
                    last_ignored.replace(addr);
                }
                return;
            }
        };

        // next event can be logged as ignored again
        last_ignored.take();

        log::trace!("{:20} <-<-<-<------ {addr} ({handle})", event.to_string());
        {
            let mut client_manager = client_manager.borrow_mut();
            let client_state = match client_manager.get_mut(handle) {
                Some(s) => s,
                None => {
                    log::error!("unknown handle");
                    return;
                }
            };

            // reset ttl for client and
            client_state.last_seen = Some(Instant::now());
            // set addr as new default for this client
            client_state.client.active_addr = Some(addr);
        }

        match (event, addr) {
            (Event::Pong(), _) => { /* ignore pong events */ }
            (Event::Ping(), addr) => {
                if let Err(e) = send_event(&socket.borrow(), Event::Pong(), addr) {
                    log::error!("udp send: {}", e);
                }
            }
            (event, addr) => {
                // tell clients that we are ready to receive events
                if let Event::Enter() = event {
                    if let Err(e) = send_event(&socket.borrow(), Event::Leave(), addr) {
                        log::error!("udp send: {}", e);
                    }
                }
                match state.get() {
                    State::Sending => {
                        if let Event::Leave() = event {
                            // ignore additional leave events that may
                            // have been sent for redundancy
                        } else {
                            // upon receiving any event, we go back to receiving mode
                            let _ = producer_notify_tx.send(ProducerEvent::Release).await;
                            state.replace(State::Receiving);
                        }
                    }
                    State::Receiving => {
                        // consume event
                        consumer.consume(event, handle).await;
                        log::trace!("{event:?} => consumer");
                    }
                    State::AwaitingLeave => {
                        // we just entered the deadzone of a client, so
                        // we need to ignore events that may still
                        // be on the way until a leave event occurs
                        // telling us the client registered the enter
                        if let Event::Leave() = event {
                            state.replace(State::Sending);
                        }

                        // entering a client that is waiting for a leave
                        // event should still be possible
                        if let Event::Enter() = event {
                            state.replace(State::Receiving);
                            let _ = producer_notify_tx.send(ProducerEvent::Release).await;
                        }
                    }
                }
            }
        }

        let mut client_manager = client_manager.borrow_mut();
        let client_state = match client_manager.get_mut(handle) {
            Some(s) => s,
            None => {
                log::error!("unknown handle");
                return;
            }
        };

        // let the server know we are still alive once every second
        if client_state.last_replied.is_none()
            || client_state.last_replied.is_some()
                && client_state.last_replied.unwrap().elapsed() > Duration::from_secs(1)
        {
            client_state.last_replied = Some(Instant::now());
            if let Err(e) = send_event(&socket.borrow(), Event::Pong(), addr) {
                log::error!("udp send: {}", e);
            }
        }
    }

    const RELEASE_MODIFIERDS: u32 = 77; // ctrl+shift+super+alt

    fn handle_producer_event(
        producer: &mut Box<dyn EventProducer>,
        client_manager: &Rc<RefCell<ClientManager>>,
        state: &Rc<Cell<State>>,
        socket: &Rc<RefCell<UdpSocket>>,
        c: ClientHandle,
        mut e: Event,
    ) {
        log::trace!("producer: ({c}) {e:?}");

        if let Event::Keyboard(crate::event::KeyboardEvent::Modifiers {
            mods_depressed,
            mods_latched: _,
            mods_locked: _,
            group: _,
        }) = e
        {
            if mods_depressed == Self::RELEASE_MODIFIERDS {
                producer.release();
                state.replace(State::Receiving);
                // send an event to release all the modifiers
                e = Event::Keyboard(KeyboardEvent::Modifiers {
                    mods_depressed: 0,
                    mods_latched: 0,
                    mods_locked: 0,
                    group: 0,
                });
            }
        }

        // get client state for handle
        let mut client_manager = client_manager.borrow_mut();
        let client_state = match client_manager.get_mut(c) {
            Some(state) => state,
            None => {
                // should not happen
                log::warn!("unknown client!");
                producer.release();
                state.replace(State::Receiving);
                return;
            }
        };

        // if we just entered the client we want to send additional enter events until
        // we get a leave event
        if let State::Receiving | State::AwaitingLeave = state.get() {
            state.replace(State::AwaitingLeave);
            if let Some(addr) = client_state.client.active_addr {
                if let Err(e) = send_event(&socket.borrow(), Event::Enter(), addr) {
                    log::error!("udp send: {}", e);
                }
            }
        }

        // otherwise we should have an address to
        // transmit events to the corrensponding client
        if let Some(addr) = client_state.client.active_addr {
            if let Err(e) = send_event(&socket.borrow(), e, addr) {
                log::error!("udp send: {}", e);
            }
        }

        // if client last responded > 2 seconds ago
        // and we have not sent a ping since 500 milliseconds, send a ping

        // check if client was seen in the past 2 seconds
        if client_state.last_seen.is_some()
            && client_state.last_seen.unwrap().elapsed() < Duration::from_secs(2)
        {
            return;
        }

        // check if last ping is < 500ms ago
        if client_state.last_ping.is_some()
            && client_state.last_ping.unwrap().elapsed() < Duration::from_millis(500)
        {
            return;
        }

        // last seen >= 2s, last ping >= 500ms
        // -> client did not respond or a ping has not been sent for a while
        // (pings are only sent when trying to access a device!)

        // check if last ping was < 1s ago -> 500ms < last_ping < 1s
        // -> client did not respond in at least 500ms
        if client_state.last_ping.is_some()
            && client_state.last_ping.unwrap().elapsed() < Duration::from_secs(1)
        {
            // client unresponsive -> set state to receiving
            if state.get() != State::Receiving {
                log::info!("client not responding - releasing pointer");
                producer.release();
                state.replace(State::Receiving);
            }
        }

        // last ping > 500ms ago -> ping all interfaces
        client_state.last_ping = Some(Instant::now());
        for addr in client_state.client.addrs.iter() {
            log::debug!("pinging {addr}");
            if let Err(e) = send_event(&socket.borrow(), Event::Ping(), *addr) {
                if e.kind() != ErrorKind::WouldBlock {
                    log::error!("udp send: {}", e);
                }
            }
        }
    }

    #[cfg(unix)]
    async fn handle_frontend_stream(
        client_manager: &Rc<RefCell<ClientManager>>,
        frontend: &mut FrontendListener,
        frontend_tx: &Sender<FrontendEvent>,
        mut stream: ReadHalf<UnixStream>,
    ) {
        use std::io;

        let tx = frontend_tx.clone();
        tokio::task::spawn_local(async move {
            loop {
                let event = frontend::read_event(&mut stream).await;
                match event {
                    Ok(event) => tx.send(event).await.unwrap(),
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
        });
        Self::enumerate(&client_manager, frontend).await;
    }

    #[cfg(windows)]
    async fn handle_frontend_stream(&mut self, mut stream: ReadHalf<TcpStream>) {
        let tx = self.frontend_tx.clone();
        tokio::task::spawn_local(async move {
            loop {
                let event = frontend::read_event(&mut stream).await;
                match event {
                    Ok(event) => tx.send(event).await.unwrap(),
                    Err(e) => log::error!("error reading frontend event: {e}"),
                }
            }
        });
        self.enumerate().await;
    }

    async fn handle_frontend_event(
        producer_notify_tx: &Sender<ProducerEvent>,
        consumer_notify_tx: &Sender<ConsumerEvent>,
        client_manager: &Rc<RefCell<ClientManager>>,
        resolve_tx: &Sender<(String, ClientHandle)>,
        frontend: &mut FrontendListener,
        socket: &Rc<RefCell<UdpSocket>>,
        event: FrontendEvent,
    ) -> bool {
        log::debug!("frontend: {event:?}");
        match event {
            FrontendEvent::AddClient(hostname, port, pos) => {
                Self::add_client(
                    &resolve_tx,
                    &client_manager,
                    frontend,
                    hostname,
                    HashSet::new(),
                    port,
                    pos,
                )
                .await;
            }
            FrontendEvent::ActivateClient(client, active) => {
                Self::activate_client(
                    &producer_notify_tx,
                    &consumer_notify_tx,
                    &client_manager,
                    client,
                    active,
                )
                .await
            }
            FrontendEvent::ChangePort(port) => {
                let current_port = socket.borrow().local_addr().unwrap().port();
                if current_port == port {
                    if let Err(e) = frontend
                        .notify_all(FrontendNotify::NotifyPortChange(port, None))
                        .await
                    {
                        log::warn!("error notifying frontend: {e}");
                    }
                    return false;
                }
                let _ = consumer_notify_tx
                    .send(ConsumerEvent::PortChange(port))
                    .await;
            }
            FrontendEvent::DelClient(client) => {
                Self::remove_client(
                    &client_manager,
                    &producer_notify_tx,
                    &consumer_notify_tx,
                    frontend,
                    client,
                )
                .await;
            }
            FrontendEvent::Enumerate() => Self::enumerate(&client_manager, frontend).await,
            FrontendEvent::Shutdown() => {
                log::info!("terminating gracefully...");
                return true;
            }
            FrontendEvent::UpdateClient(client, hostname, port, pos) => {
                Self::update_client(
                    &producer_notify_tx,
                    &consumer_notify_tx,
                    resolve_tx,
                    &client_manager,
                    client,
                    hostname,
                    port,
                    pos,
                )
                .await
            }
        }
        false
    }

    async fn enumerate(
        client_manager: &Rc<RefCell<ClientManager>>,
        frontend: &mut FrontendListener,
    ) {
        let clients = client_manager
            .borrow()
            .get_client_states()
            .map(|s| (s.client.clone(), s.active))
            .collect();
        if let Err(e) = frontend
            .notify_all(FrontendNotify::Enumerate(clients))
            .await
        {
            log::error!("error notifying frontend: {e}");
        }
    }
}

async fn receive_event(
    socket: &Rc<RefCell<UdpSocket>>,
) -> std::result::Result<(Event, SocketAddr), Box<dyn Error>> {
    let socket = socket.borrow();
    let mut buf = vec![0u8; 22];
    match socket.recv_from(&mut buf).await {
        Ok((_amt, src)) => Ok((Event::try_from(buf)?, src)),
        Err(e) => Err(Box::new(e)),
    }
}

fn send_event(sock: &UdpSocket, e: Event, addr: SocketAddr) -> Result<usize> {
    log::trace!("{:20} ------>->->-> {addr}", e.to_string());
    let data: Vec<u8> = (&e).into();
    // When udp blocks, we dont want to block the event loop.
    // Dropping events is better than potentially crashing the event
    // producer.
    sock.try_send_to(&data, addr)
}
