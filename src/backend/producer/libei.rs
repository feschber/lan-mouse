use anyhow::{anyhow, Result};
use ashpd::desktop::{
    input_capture::{Barrier, BarrierID, Capabilities, InputCapture, Region, Zones},
    Session,
};
use futures::StreamExt;
use reis::{
    ei::{self, keyboard::KeyState},
    eis::button::ButtonState,
    event::{DeviceCapability, EiEvent},
    tokio::{EiConvertEventStream, EiEventStream},
};
use std::{
    collections::HashMap,
    io,
    os::unix::net::UnixStream,
    pin::Pin,
    task::{ready, Context, Poll},
};
use tokio::{sync::mpsc::Sender, task::JoinHandle};

use futures_core::Stream;
use once_cell::sync::Lazy;

use crate::{
    client::{ClientEvent, ClientHandle, Position},
    event::{Event, KeyboardEvent, PointerEvent},
    producer::EventProducer,
};

#[derive(Debug)]
enum ProducerEvent {
    Release,
    ClientEvent(ClientEvent),
}

#[allow(dead_code)]
pub struct LibeiProducer {
    libei_task: JoinHandle<Result<()>>,
    event_rx: tokio::sync::mpsc::Receiver<(u32, Event)>,
    notify_tx: tokio::sync::mpsc::Sender<ProducerEvent>,
}

static INTERFACES: Lazy<HashMap<&'static str, u32>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("ei_connection", 1);
    m.insert("ei_callback", 1);
    m.insert("ei_pingpong", 1);
    m.insert("ei_seat", 1);
    m.insert("ei_device", 2);
    m.insert("ei_pointer", 1);
    m.insert("ei_pointer_absolute", 1);
    m.insert("ei_scroll", 1);
    m.insert("ei_button", 1);
    m.insert("ei_keyboard", 1);
    m.insert("ei_touchscreen", 1);
    m
});

fn pos_to_barrier(r: &Region, pos: Position) -> (i32, i32, i32, i32) {
    let (x, y) = (r.x_offset(), r.y_offset());
    let (width, height) = (r.width() as i32, r.height() as i32);
    match pos {
        Position::Left => (x, y, x, y + height - 1), // start pos, end pos, inclusive
        Position::Right => (x + width, y, x + width, y + height - 1),
        Position::Top => (x, y, x + width - 1, y),
        Position::Bottom => (x, y + height, x + width - 1, y + height),
    }
}

fn select_barriers(
    zones: &Zones,
    clients: &Vec<(ClientHandle, Position)>,
    next_barrier_id: &mut u32,
) -> (Vec<Barrier>, HashMap<BarrierID, ClientHandle>) {
    let mut client_for_barrier = HashMap::new();
    let mut barriers: Vec<Barrier> = vec![];

    for (handle, pos) in clients {
        let mut client_barriers = zones
            .regions()
            .iter()
            .map(|r| {
                let id = *next_barrier_id;
                *next_barrier_id = id + 1;
                let position = pos_to_barrier(r, *pos);
                client_for_barrier.insert(id, *handle);
                Barrier::new(id, position)
            })
            .collect();
        barriers.append(&mut client_barriers);
    }
    (barriers, client_for_barrier)
}

async fn update_barriers(
    input_capture: &InputCapture<'_>,
    session: &Session<'_>,
    active_clients: &Vec<(ClientHandle, Position)>,
    client_for_barrier_id: &mut HashMap<BarrierID, ClientHandle>,
    next_barrier_id: &mut u32,
) -> Result<()> {
    input_capture.disable(session).await?;

    log::debug!("selecting zones");
    let zones = input_capture.zones(session).await?.response()?;
    log::debug!("{zones:?}");

    let (barriers, new_map) = select_barriers(&zones, active_clients, next_barrier_id);
    *client_for_barrier_id = new_map;

    log::debug!("selecting barriers: {barriers:?}");
    let response = input_capture
        .set_pointer_barriers(session, &barriers, zones.zone_set())
        .await?;
    let response = response.response()?;
    log::debug!("response: {response:?}");
    input_capture.enable(session).await?;
    Ok(())
}

impl LibeiProducer {
    pub async fn new() -> Result<Self> {
        // connect to eis for input capture
        log::debug!("creating input capture proxy");
        let input_capture = InputCapture::new().await?;
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(32);
        let (notify_tx, mut notify_rx) = tokio::sync::mpsc::channel(32);
        let libei_task = tokio::task::spawn_local(async move {
            // create input capture session
            log::debug!("creating input capture session");
            let (session, _cap) = input_capture
                .create_session(
                    &ashpd::WindowIdentifier::default(),
                    Capabilities::Keyboard | Capabilities::Pointer | Capabilities::Touchscreen,
                )
                .await?;

            // connect to eis server
            log::debug!("connect_to_eis");
            let fd = input_capture.connect_to_eis(&session).await?;

            // create unix stream from fd
            let stream = UnixStream::from(fd);
            stream.set_nonblocking(true)?;

            // create ei context
            let context = ei::Context::new(stream)?;
            let mut event_stream = EiEventStream::new(context.clone())?;
            let _handshake = match reis::tokio::ei_handshake(
                &mut event_stream,
                "lan-mouse",
                ei::handshake::ContextType::Receiver,
                &INTERFACES,
            )
            .await
            {
                Ok(res) => res,
                Err(e) => return Err(anyhow!("ei handshake failed: {e:?}")),
            };

            let mut event_stream = EiConvertEventStream::new(event_stream);

            let mut active_clients: Vec<(ClientHandle, Position)> = vec![];
            let mut client_for_barrier_id: HashMap<BarrierID, ClientHandle> = HashMap::new();
            let mut next_barrier_id = 1u32;

            log::debug!("enabling session");
            input_capture.enable(&session).await?;

            let mut activated = input_capture.receive_activated().await?;

            loop {
                let (activated, current_client) = {
                    log::debug!("receiving activation token");
                    tokio::select! {
                        activated = activated.next() => {
                            let activated = activated.ok_or(anyhow!("error receiving activation token"))?;
                            log::debug!("activation token: {activated:?}");
                            let current_client = match client_for_barrier_id.get(&activated.barrier_id()) {
                                None => {
                                    log::warn!("invalid barrier id!");
                                    continue;
                                }
                                Some(c) => *c,
                            };
                            event_tx.send((current_client, Event::Enter())).await?;
                            tokio::select! {
                                res = do_capture(&context, &mut event_stream, current_client, event_tx.clone()) => {
                                    res?;
                                }
                                producer_event = notify_rx.recv() => {
                                    let producer_event = match producer_event {
                                        Some(e) => e,
                                        None => continue,
                                    };
                                    handle_producer_event(producer_event, &input_capture, &session, &mut next_barrier_id, &mut client_for_barrier_id, &mut active_clients).await?;
                                }
                            }
                            (activated, current_client)
                        }
                        producer_event = notify_rx.recv() => {
                            let producer_event = match producer_event {
                                Some(e) => e,
                                None => continue,
                            };
                            handle_producer_event(producer_event, &input_capture, &session, &mut next_barrier_id, &mut client_for_barrier_id, &mut active_clients).await?;
                            continue;
                        },
                    }
                };

                log::debug!("releasing input capture {}", activated.activation_id());
                let (x, y) = activated.cursor_position();
                let pos = active_clients
                    .iter()
                    .filter(|(c, _)| *c == current_client)
                    .map(|(_, p)| p)
                    .next()
                    .unwrap(); // FIXME
                let (dx, dy) = match pos {
                    // offset cursor position to not enter again immediately
                    Position::Left => (1., 0.),
                    Position::Right => (-1., 0.),
                    Position::Top => (0., 1.),
                    Position::Bottom => (0., -1.),
                };
                // release 1px to the right of the entered zone
                let cursor_position = (x as f64 + dx, y as f64 + dy);
                input_capture
                    .release(&session, activated.activation_id(), cursor_position)
                    .await?;
            }
        });

        let producer = Self {
            event_rx,
            libei_task,
            notify_tx,
        };

        Ok(producer)
    }
}

async fn do_capture(
    context: &ei::Context,
    event_stream: &mut EiConvertEventStream,
    current_client: ClientHandle,
    event_tx: Sender<(ClientHandle, Event)>,
) -> Result<()> {
    loop {
        let ei_event = match event_stream.next().await {
            Some(Ok(event)) => event,
            Some(Err(e)) => return Err(anyhow!("libei connection closed: {e:?}")),
            None => return Err(anyhow!("libei connection closed")),
        };
        let lan_mouse_event = to_lan_mouse_event(ei_event, context);
        if let Some(event) = lan_mouse_event {
            let _ = event_tx.send((current_client, event)).await;
        }
    }
}

async fn handle_producer_event(
    producer_event: ProducerEvent,
    input_capture: &InputCapture<'_>,
    session: &Session<'_>,
    next_barrier_id: &mut u32,
    client_for_barrier_id: &mut HashMap<BarrierID, ClientHandle>,
    active_clients: &mut Vec<(ClientHandle, Position)>,
) -> Result<()> {
    log::debug!("handling event: {producer_event:?}");
    match producer_event {
        ProducerEvent::Release => {}
        ProducerEvent::ClientEvent(c) => match c {
            ClientEvent::Create(c, p) => {
                active_clients.push((c, p));
                update_barriers(
                    input_capture,
                    session,
                    active_clients,
                    client_for_barrier_id,
                    next_barrier_id,
                )
                .await?;
                log::debug!("client for barrier id: {client_for_barrier_id:?}");
            }
            ClientEvent::Destroy(c) => {
                active_clients.retain(|(h, _)| *h != c);
                update_barriers(
                    input_capture,
                    session,
                    active_clients,
                    client_for_barrier_id,
                    next_barrier_id,
                )
                .await?;
                log::debug!("client for barrier id: {client_for_barrier_id:?}");
            }
        },
    }
    Ok(())
}

fn to_lan_mouse_event(ei_event: EiEvent, context: &ei::Context) -> Option<Event> {
    match ei_event {
        EiEvent::SeatAdded(seat_event) => {
            seat_event.seat.bind_capabilities(&[
                DeviceCapability::Pointer,
                DeviceCapability::PointerAbsolute,
                DeviceCapability::Keyboard,
                DeviceCapability::Touch,
                DeviceCapability::Scroll,
                DeviceCapability::Button,
            ]);
            let _ = context.flush();
            None
        }
        EiEvent::SeatRemoved(_) => None,
        EiEvent::DeviceAdded(_) => None,
        EiEvent::DeviceRemoved(_) => None,
        EiEvent::DevicePaused(_) => None,
        EiEvent::DeviceResumed(_) => None,
        EiEvent::KeyboardModifiers(mods) => {
            let modifier_event = KeyboardEvent::Modifiers {
                mods_depressed: mods.depressed,
                mods_latched: mods.latched,
                mods_locked: mods.locked,
                group: mods.group,
            };
            Some(Event::Keyboard(modifier_event))
        }
        EiEvent::Frame(_) => None,
        EiEvent::DeviceStartEmulating(_) => None,
        EiEvent::DeviceStopEmulating(_) => None,
        EiEvent::PointerMotion(motion) => {
            let motion_event = PointerEvent::Motion {
                time: 0,
                relative_x: motion.dx as f64,
                relative_y: motion.dy as f64,
            };
            Some(Event::Pointer(motion_event))
        }
        EiEvent::PointerMotionAbsolute(_) => None,
        EiEvent::Button(button) => {
            let button_event = PointerEvent::Button {
                time: button.time as u32,
                button: button.button,
                state: match button.state {
                    ButtonState::Released => 0,
                    ButtonState::Press => 1,
                },
            };
            Some(Event::Pointer(button_event))
        }
        EiEvent::ScrollDelta(_) => None,
        EiEvent::ScrollStop(_) => None,
        EiEvent::ScrollCancel(_) => None,
        EiEvent::ScrollDiscrete(scroll) => {
            let axis_event = if scroll.discrete_dy > 0 {
                PointerEvent::Axis {
                    time: 0,
                    axis: 0,
                    value: scroll.discrete_dy as f64,
                }
            } else {
                PointerEvent::Axis {
                    time: 0,
                    axis: 1,
                    value: scroll.discrete_dx as f64,
                }
            };
            Some(Event::Pointer(axis_event))
        }
        EiEvent::KeyboardKey(key) => {
            let key_event = KeyboardEvent::Key {
                key: key.key,
                state: match key.state {
                    KeyState::Press => 1,
                    KeyState::Released => 0,
                },
                time: key.time as u32,
            };
            Some(Event::Keyboard(key_event))
        }
        EiEvent::TouchDown(_) => None,
        EiEvent::TouchUp(_) => None,
        EiEvent::TouchMotion(_) => None,
    }
}

impl EventProducer for LibeiProducer {
    fn notify(&mut self, event: ClientEvent) -> io::Result<()> {
        let notify_tx = self.notify_tx.clone();
        tokio::task::spawn_local(async move {
            log::debug!("notifying {event:?}");
            let _ = notify_tx.send(ProducerEvent::ClientEvent(event)).await;
            log::debug!("done !");
        });
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        let notify_tx = self.notify_tx.clone();
        tokio::task::spawn_local(async move {
            log::debug!("notifying Release");
            let _ = notify_tx.send(ProducerEvent::Release).await;
        });
        Ok(())
    }
}

impl Stream for LibeiProducer {
    type Item = io::Result<(ClientHandle, Event)>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match ready!(self.event_rx.poll_recv(cx)) {
            None => Poll::Ready(None),
            Some(e) => Poll::Ready(Some(Ok(e))),
        }
    }
}
