use std::{net::SocketAddr, time::Duration};

use tokio::{sync::mpsc::Sender, task::JoinHandle};

use input_event::Event;

use crate::client::ClientHandle;

use super::{capture_task::CaptureEvent, emulation_task::EmulationEvent, Server, State};

const MAX_RESPONSE_TIME: Duration = Duration::from_millis(500);

pub(crate) fn new(
    server: Server,
    sender_ch: Sender<(Event, SocketAddr)>,
    emulate_notify: Sender<EmulationEvent>,
    capture_notify: Sender<CaptureEvent>,
) -> JoinHandle<()> {
    // timer task
    tokio::task::spawn_local(async move {
        tokio::select! {
            _ = server.notifies.cancel.cancelled() => {}
            _ = ping_task(&server, sender_ch, emulate_notify, capture_notify) => {}
        }
    })
}

async fn ping_task(
    server: &Server,
    sender_ch: Sender<(Event, SocketAddr)>,
    emulate_notify: Sender<EmulationEvent>,
    capture_notify: Sender<CaptureEvent>,
) {
    loop {
        // wait for wake up signal
        server.ping_timer_notified().await;
        loop {
            let receiving = server.state.get() == State::Receiving;
            let (ping_clients, ping_addrs) = {
                let mut client_manager = server.client_manager.borrow_mut();

                let ping_clients: Vec<ClientHandle> = if receiving {
                    // if receiving we care about clients with pressed keys
                    client_manager
                        .get_client_states_mut()
                        .filter(|(_, (_, s))| !s.pressed_keys.is_empty())
                        .map(|(h, _)| h)
                        .collect()
                } else {
                    // if sending we care about the active client
                    server.active_client.get().iter().cloned().collect()
                };

                // get relevant socket addrs for clients
                let ping_addrs: Vec<SocketAddr> = {
                    ping_clients
                        .iter()
                        .flat_map(|&h| client_manager.get(h))
                        .flat_map(|(c, s)| {
                            if s.alive && s.active_addr.is_some() {
                                vec![s.active_addr.unwrap()]
                            } else {
                                s.ips
                                    .iter()
                                    .cloned()
                                    .map(|ip| SocketAddr::new(ip, c.port))
                                    .collect()
                            }
                        })
                        .collect()
                };

                // reset alive
                for (_, (_, s)) in client_manager.get_client_states_mut() {
                    s.alive = false;
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
                log::trace!(
                    "waiting {MAX_RESPONSE_TIME:?} for response from client with pressed keys ..."
                );
            } else {
                log::trace!(
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
                    .filter_map(|&h| match client_manager.get(h) {
                        Some((_, s)) if !s.alive => Some(h),
                        _ => None,
                    })
                    .collect()
            };

            // we may not be receiving anymore but we should respond
            // to the original state and not the "new" one
            if receiving {
                for h in unresponsive_clients {
                    log::warn!("device not responding, releasing keys!");
                    let _ = emulate_notify.send(EmulationEvent::ReleaseKeys(h)).await;
                }
            } else {
                // release pointer if the active client has not responded
                if !unresponsive_clients.is_empty() {
                    log::warn!("client not responding, releasing pointer!");
                    server.state.replace(State::Receiving);
                    let _ = capture_notify.send(CaptureEvent::Release).await;
                }
            }
        }
    }
}
