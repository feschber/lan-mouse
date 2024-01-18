use anyhow::{anyhow, Result};
use futures::StreamExt;
use std::net::SocketAddr;

use tokio::{sync::mpsc::Sender, task::JoinHandle};

use crate::{
    client::{ClientEvent, ClientHandle},
    event::{Event, KeyboardEvent},
    producer::EventProducer,
    server::State,
};

use super::Server;

#[derive(Clone, Copy, Debug)]
pub enum ProducerEvent {
    /// producer must release the mouse
    Release,
    /// producer is notified of a change in client states
    ClientEvent(ClientEvent),
    /// termination signal
    Terminate,
}

pub fn new(
    mut producer: Box<dyn EventProducer>,
    server: Server,
    sender_tx: Sender<(Event, SocketAddr)>,
    timer_tx: Sender<()>,
) -> (JoinHandle<Result<()>>, Sender<ProducerEvent>) {
    let (tx, mut rx) = tokio::sync::mpsc::channel(32);
    let task = tokio::task::spawn_local(async move {
        loop {
            tokio::select! {
                event = producer.next() => {
                    let event = event.ok_or(anyhow!("event producer closed"))??;
                    handle_producer_event(&server, &mut producer, &sender_tx, &timer_tx, event).await?;
                }
                e = rx.recv() => {
                    log::debug!("producer notify rx: {e:?}");
                    match e {
                        Some(e) => match e {
                            ProducerEvent::Release => {
                                producer.release()?;
                                server.state.replace(State::Receiving);

                            }
                            ProducerEvent::ClientEvent(e) => producer.notify(e)?,
                            ProducerEvent::Terminate => break,
                        },
                        None => break,
                    }
                }
            }
        }
        anyhow::Ok(())
    });
    (task, tx)
}

const RELEASE_MODIFIERDS: u32 = 77; // ctrl+shift+super+alt

async fn handle_producer_event(
    server: &Server,
    producer: &mut Box<dyn EventProducer>,
    sender_tx: &Sender<(Event, SocketAddr)>,
    timer_tx: &Sender<()>,
    event: (ClientHandle, Event),
) -> Result<()> {
    let (c, mut e) = event;
    log::trace!("({c}) {e:?}");

    if let Event::Keyboard(KeyboardEvent::Modifiers { mods_depressed, .. }) = e {
        if mods_depressed == RELEASE_MODIFIERDS {
            producer.release()?;
            server.state.replace(State::Receiving);
            log::trace!("STATE ===> Receiving");
            // send an event to release all the modifiers
            e = Event::Disconnect();
        }
    }

    let (addr, enter, start_timer) = {
        let mut enter = false;
        let mut start_timer = false;

        // get client state for handle
        let mut client_manager = server.client_manager.borrow_mut();
        let client_state = match client_manager.get_mut(c) {
            Some(state) => state,
            None => {
                // should not happen
                log::warn!("unknown client!");
                producer.release()?;
                server.state.replace(State::Receiving);
                log::trace!("STATE ===> Receiving");
                return Ok(());
            }
        };

        // if we just entered the client we want to send additional enter events until
        // we get a leave event
        if let Event::Enter() = e {
            server.state.replace(State::AwaitingLeave);
            server
                .active_client
                .replace(Some(client_state.client.handle));
            log::trace!("Active client => {}", client_state.client.handle);
            start_timer = true;
            log::trace!("STATE ===> AwaitingLeave");
            enter = true;
        } else {
            // ignore any potential events in receiving mode
            if server.state.get() == State::Receiving && e != Event::Disconnect() {
                return Ok(());
            }
        }

        (client_state.active_addr, enter, start_timer)
    };
    if start_timer {
        let _ = timer_tx.try_send(());
    }
    if let Some(addr) = addr {
        if enter {
            let _ = sender_tx.send((Event::Enter(), addr)).await;
        }
        let _ = sender_tx.send((e, addr)).await;
    }
    Ok(())
}
