use anyhow::{anyhow, Result};
use std::net::SocketAddr;

use tokio::{
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};

use crate::{
    client::{ClientEvent, ClientHandle},
    consumer::EventConsumer,
    event::{Event, KeyboardEvent},
    scancode,
    server::State,
};

use super::{ProducerEvent, Server};

#[derive(Clone, Debug)]
pub enum ConsumerEvent {
    /// consumer is notified of a change in client states
    ClientEvent(ClientEvent),
    /// consumer must release keys for client
    ReleaseKeys(ClientHandle),
    /// termination signal
    Terminate,
}

pub fn new(
    mut consumer: Box<dyn EventConsumer>,
    server: Server,
    mut udp_rx: Receiver<Result<(Event, SocketAddr)>>,
    sender_tx: Sender<(Event, SocketAddr)>,
    producer_tx: Sender<ProducerEvent>,
    timer_tx: Sender<()>,
) -> (JoinHandle<Result<()>>, Sender<ConsumerEvent>) {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let consumer_task = tokio::task::spawn_local(async move {
        let mut last_ignored = None;

        loop {
            tokio::select! {
                udp_event = udp_rx.recv() => {
                    let udp_event = udp_event.ok_or(anyhow!("receiver closed"))??;
                    handle_udp_rx(&server, &producer_tx, &mut consumer, &sender_tx, &mut last_ignored, udp_event, &timer_tx).await;
                }
                consumer_event = rx.recv() => {
                    match consumer_event {
                        Some(e) => match e {
                            ConsumerEvent::ClientEvent(e) => consumer.notify(e).await,
                            ConsumerEvent::ReleaseKeys(c) => release_keys(&server, &mut consumer, c).await,
                            ConsumerEvent::Terminate => break,
                        },
                        None => break,
                    }
                }
                res = consumer.dispatch() => {
                    res?;
                }
            }
        }

        // release potentially still pressed keys
        let clients = server
            .client_manager
            .borrow()
            .get_client_states()
            .map(|s| s.client.handle)
            .collect::<Vec<_>>();
        for client in clients {
            release_keys(&server, &mut consumer, client).await;
        }

        // destroy consumer
        consumer.destroy().await;
        anyhow::Ok(())
    });
    (consumer_task, tx)
}

async fn handle_udp_rx(
    server: &Server,
    producer_notify_tx: &Sender<ProducerEvent>,
    consumer: &mut Box<dyn EventConsumer>,
    sender_tx: &Sender<(Event, SocketAddr)>,
    last_ignored: &mut Option<SocketAddr>,
    event: (Event, SocketAddr),
    timer_tx: &Sender<()>,
) {
    let (event, addr) = event;

    // get handle for addr
    let handle = match server.client_manager.borrow().get_client(addr) {
        Some(a) => a,
        None => {
            if last_ignored.is_none() || last_ignored.is_some() && last_ignored.unwrap() != addr {
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
        let mut client_manager = server.client_manager.borrow_mut();
        let client_state = match client_manager.get_mut(handle) {
            Some(s) => s,
            None => {
                log::error!("unknown handle");
                return;
            }
        };

        // reset ttl for client and
        client_state.alive = true;
        // set addr as new default for this client
        client_state.active_addr = Some(addr);
    }

    match (event, addr) {
        (Event::Pong(), _) => { /* ignore pong events */ }
        (Event::Ping(), addr) => {
            let _ = sender_tx.send((Event::Pong(), addr)).await;
        }
        (Event::Disconnect(), _) => {
            release_keys(server, consumer, handle).await;
        }
        (event, addr) => {
            // tell clients that we are ready to receive events
            if let Event::Enter() = event {
                let _ = sender_tx.send((Event::Leave(), addr)).await;
            }

            match server.state.get() {
                State::Sending => {
                    if let Event::Leave() = event {
                        // ignore additional leave events that may
                        // have been sent for redundancy
                    } else {
                        // upon receiving any event, we go back to receiving mode
                        server.state.replace(State::Receiving);
                        let _ = producer_notify_tx.send(ProducerEvent::Release).await;
                        log::trace!("STATE ===> Receiving");
                    }
                }
                State::Receiving => {
                    let mut ignore_event = false;
                    if let Event::Keyboard(KeyboardEvent::Key {
                        time: _,
                        key,
                        state,
                    }) = event
                    {
                        let mut client_manager = server.client_manager.borrow_mut();
                        let client_state =
                            if let Some(client_state) = client_manager.get_mut(handle) {
                                client_state
                            } else {
                                log::error!("unknown handle");
                                return;
                            };
                        if state == 0 {
                            // ignore release event if key not pressed
                            ignore_event = !client_state.pressed_keys.remove(&key);
                        } else {
                            // ignore press event if key not released
                            ignore_event = !client_state.pressed_keys.insert(key);
                            let _ = timer_tx.try_send(());
                        }
                    }
                    // ignore double press / release events to
                    // workaround buggy rdp backend.
                    if !ignore_event {
                        // consume event
                        consumer.consume(event, handle).await;
                        log::trace!("{event:?} => consumer");
                    }
                }
                State::AwaitingLeave => {
                    // we just entered the deadzone of a client, so
                    // we need to ignore events that may still
                    // be on the way until a leave event occurs
                    // telling us the client registered the enter
                    if let Event::Leave() = event {
                        server.state.replace(State::Sending);
                        log::trace!("STATE ===> Sending");
                    }

                    // entering a client that is waiting for a leave
                    // event should still be possible
                    if let Event::Enter() = event {
                        server.state.replace(State::Receiving);
                        let _ = producer_notify_tx.send(ProducerEvent::Release).await;
                        log::trace!("STATE ===> Receiving");
                    }
                }
            }
        }
    }
}

async fn release_keys(
    server: &Server,
    consumer: &mut Box<dyn EventConsumer>,
    client: ClientHandle,
) {
    let keys = server
        .client_manager
        .borrow_mut()
        .get_mut(client)
        .iter_mut()
        .flat_map(|s| s.pressed_keys.drain())
        .collect::<Vec<_>>();

    for key in keys {
        let event = Event::Keyboard(KeyboardEvent::Key {
            time: 0,
            key,
            state: 0,
        });
        consumer.consume(event, client).await;
        if let Ok(key) = scancode::Linux::try_from(key) {
            log::warn!("releasing stuck key: {key:?}");
        }
    }

    let modifiers_event = KeyboardEvent::Modifiers {
        mods_depressed: 0,
        mods_latched: 0,
        mods_locked: 0,
        group: 0,
    };
    consumer
        .consume(Event::Keyboard(modifiers_event), client)
        .await;
}
