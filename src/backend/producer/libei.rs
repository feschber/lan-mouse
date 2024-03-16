use anyhow::{anyhow, Result};
use ashpd::desktop::input_capture::{Barrier, Capabilities, InputCapture, Zones};
use futures::StreamExt;
use reis::{
    ei::{self, keyboard::KeyState},
    eis::button::ButtonState,
    event::{DeviceCapability, EiEvent},
    tokio::{EiConvertEventStream, EiEventStream},
};
use tokio::task::JoinHandle;
use std::{
    io,
    os::unix::net::UnixStream,
    pin::Pin,
    task::{ready, Context, Poll}, collections::HashMap,
};

use futures_core::Stream;
use once_cell::sync::Lazy;

use crate::{
    client::{ClientEvent, ClientHandle, Position},
    event::{Event, KeyboardEvent, PointerEvent},
    producer::EventProducer,
};

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

fn select_barriers(zones: &Zones, pos: Position) -> Vec<Barrier> {
    zones
        .regions()
        .iter()
        .enumerate()
        .map(|(n, r)| {
            let id = n as u32;
            let (x, y) = (r.x_offset(), r.y_offset());
            let (width, height) = (r.width() as i32, r.height() as i32);
            let barrier_pos = match pos {
                Position::Left => (x, y, x, y + height - 1), // start pos, end pos, inclusive
                Position::Right => (x + width - 1, y, x + width - 1, y + height - 1),
                Position::Top => (x, y, x + width - 1, y),
                Position::Bottom => (x, y + height - 1, x + width - 1, y + height - 1),
            };
            Barrier::new(id, barrier_pos)
        })
        .collect()
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
                    (Capabilities::Keyboard | Capabilities::Pointer | Capabilities::Touchscreen).into(),
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
            ).await {
                Ok(res) => res,
                Err(e) => return Err(anyhow!("ei handshake failed: {e:?}")),
            };

            let mut event_stream = EiConvertEventStream::new(event_stream);


            log::debug!("selecting zones");
            let zones = input_capture.zones(&session).await?.response()?;
            log::debug!("{zones:?}");
            // FIXME: position
            let barriers = select_barriers(&zones, Position::Left);

            log::debug!("selecting barriers: {barriers:?}");
            input_capture
                .set_pointer_barriers(&session, &barriers, zones.zone_set())
                .await?;

            log::debug!("enabling session");
            input_capture.enable(&session).await?;

            let mut activated = input_capture.receive_activated().await?;

            loop {
                log::debug!("receiving activation token");
                let activated = activated.next().await.ok_or(anyhow!("error receiving activation token"))?;
                log::debug!("activation token: {activated:?}");


                let mut entered = false;
                loop {
                    tokio::select! { biased;
                        ei_event = event_stream.next() => {
                            let ei_event = match ei_event {
                                Some(Ok(e)) => e,
                                _ => return Ok(()),
                            };
                            let lan_mouse_event = to_lan_mouse_event(ei_event, &context);
                            if !entered {
                                // FIXME
                                let _ = event_tx.send((0, Event::Enter())).await;
                                entered = true;
                            }
                            if let Some(event) = lan_mouse_event {
                                let _ = event_tx.send(event).await;
                            }
                        }
                        producer_event = notify_rx.recv() => {
                            let producer_event = match producer_event {
                                Some(e) => e,
                                None => continue,
                            };
                            match producer_event {
                                ProducerEvent::Release => break,
                                ProducerEvent::ClientEvent(_) => { log::warn!("TODO") },
                            }
                        },
                    }
                }
                log::debug!("releasing input capture");
                let (x, y) = activated.cursor_position();
                 // release 1px to the right of the entered zone
                let cursor_position = (x as f64 + 1., y as f64);
                input_capture.release(&session, activated.activation_id(), cursor_position).await.unwrap(); // FIXME
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

fn to_lan_mouse_event(ei_event: EiEvent, context: &ei::Context) -> Option<(ClientHandle, Event)> {
    let client = 0; // FIXME
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
            Some((client, Event::Keyboard(modifier_event)))
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
            Some((client, Event::Pointer(motion_event)))
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
            Some((client, Event::Pointer(button_event)))
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
            Some((client, Event::Pointer(axis_event)))
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
            Some((client, Event::Keyboard(key_event)))
        }
        EiEvent::TouchDown(_) => None,
        EiEvent::TouchUp(_) => None,
        EiEvent::TouchMotion(_) => None,
    }
}

impl EventProducer for LibeiProducer {
    fn notify(&mut self, event: ClientEvent) -> io::Result<()> {
        let notify_tx = self.notify_tx.clone(); // FIXME
        tokio::task::spawn_local(async move {
            notify_tx.send(ProducerEvent::ClientEvent(event)).await.unwrap(); // FIXME
        });
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        let notify_tx = self.notify_tx.clone(); // FIXME
        tokio::task::spawn_local(async move {
            notify_tx.send(ProducerEvent::Release).await.unwrap(); // FIXME
        });
        Ok(())
    }
}

impl Stream for LibeiProducer {
    type Item = io::Result<(ClientHandle, Event)>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match ready!(self.event_rx.poll_recv(cx)) {
            None => return Poll::Ready(None),
            Some(e) => return Poll::Ready(Some(Ok(e))),
        }
    }
}
