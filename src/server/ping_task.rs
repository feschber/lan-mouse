use std::{net::SocketAddr, time::Duration};

use tokio::{
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};

use crate::{client::ClientHandle, event::Event};

use super::{consumer_task::ConsumerEvent, producer_task::ProducerEvent, Server, State};

const MAX_RESPONSE_TIME: Duration = Duration::from_millis(500);

pub fn new(
    server: Server,
    sender_ch: Sender<(Event, SocketAddr)>,
    consumer_notify: Sender<ConsumerEvent>,
    producer_notify: Sender<ProducerEvent>,
    mut timer_rx: Receiver<()>,
) -> JoinHandle<()> {
    // timer task
    let ping_task = tokio::task::spawn_local(async move {
        loop {
            // wait for wake up signal
            let Some(_): Option<()> = timer_rx.recv().await else {
                break;
            };
            loop {
                let receiving = server.state.get() == State::Receiving;
                let (ping_clients, ping_addrs) = {
                    let mut client_manager = server.client_manager.borrow_mut();

                    let ping_clients: Vec<ClientHandle> = if receiving {
                        // if receiving we care about clients with pressed keys
                        client_manager
                            .get_client_states_mut()
                            .filter(|s| !s.pressed_keys.is_empty())
                            .map(|s| s.client.handle)
                            .collect()
                    } else {
                        // if sending we care about the active client
                        server.active_client.get().iter().cloned().collect()
                    };

                    // get relevant socket addrs for clients
                    let ping_addrs: Vec<SocketAddr> = {
                        ping_clients
                            .iter()
                            .flat_map(|&c| client_manager.get(c))
                            .flat_map(|state| {
                                if state.alive && state.active_addr.is_some() {
                                    vec![state.active_addr.unwrap()]
                                } else {
                                    state
                                        .client
                                        .ips
                                        .iter()
                                        .cloned()
                                        .map(|ip| SocketAddr::new(ip, state.client.port))
                                        .collect()
                                }
                            })
                            .collect()
                    };

                    // reset alive
                    for state in client_manager.get_client_states_mut() {
                        state.alive = false;
                    }

                    (ping_clients, ping_addrs)
                };

                if receiving && ping_clients.is_empty() {
                    // receiving and no client has pressed keys
                    // -> no need to keep pinging
                    break;
                }

                // ping clients
                for addr in ping_addrs {
                    if sender_ch.send((Event::Ping(), addr)).await.is_err() {
                        break;
                    }
                }

                // give clients time to resond
                if receiving {
                    log::debug!("waiting {MAX_RESPONSE_TIME:?} for response from client with pressed keys ...");
                } else {
                    log::debug!(
                        "state: {:?} => waiting {MAX_RESPONSE_TIME:?} for client to respond ...",
                        server.state.get()
                    );
                }

                tokio::time::sleep(MAX_RESPONSE_TIME).await;

                // when anything is received from a client,
                // the alive flag gets set
                let unresponsive_clients: Vec<_> = {
                    let client_manager = server.client_manager.borrow();
                    ping_clients
                        .iter()
                        .filter_map(|&c| match client_manager.get(c) {
                            Some(state) if !state.alive => Some(c),
                            _ => None,
                        })
                        .collect()
                };

                // we may not be receiving anymore but we should respond
                // to the original state and not the "new" one
                if receiving {
                    for c in unresponsive_clients {
                        log::warn!("device not responding, releasing keys!");
                        let _ = consumer_notify.send(ConsumerEvent::ReleaseKeys(c)).await;
                    }
                } else {
                    // release pointer if the active client has not responded
                    if !unresponsive_clients.is_empty() {
                        log::warn!("client not responding, releasing pointer!");
                        server.state.replace(State::Receiving);
                        let _ = producer_notify.send(ProducerEvent::Release).await;
                    }
                }
            }
        }
    });
    ping_task
}
