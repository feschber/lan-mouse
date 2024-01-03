use anyhow::{anyhow, Result};
use ashpd::desktop::{input_capture::{Barrier, Capabilities, InputCapture, Zones}, Session};
use futures::StreamExt;
use reis::{
    ei::{self, keyboard::KeyState},
    eis::button::ButtonState,
    event::{DeviceCapability, EiEvent, Device},
    tokio::{EiConvertEventStream, EiEventStream},
};
use std::{
    io,
    os::{fd::FromRawFd, unix::net::UnixStream},
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

#[allow(dead_code)]
pub struct LibeiProducer {
    context: ei::Context,
    event_stream: EiConvertEventStream,
    // input_capture: InputCapture<'a>,
    // session: Session<'a>,
    zones: Zones,
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
        let eifd = unsafe {
            let fd = libc::dup(fd);
            if fd < 0 {
                return Err(anyhow!(
                    "failed to dup eifd: {}",
                    io::Error::last_os_error()
                ));
            } else {
                fd
            }
        };

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
        // let activated = match input_capture.receive_activated().await?.next().await.ok_or("could not receive activation_signal") {
            // Ok(s) => Ok(s),
            // Err(s) => Err(anyhow!("failed to receive activation token: {s}")),
        // }?;
        // log::debug!("received activation: {activated:?}");
        // activated.activation_id();

        // create unix stream from fd
        let stream = unsafe { UnixStream::from_raw_fd(eifd) };
        stream.set_nonblocking(true)?;

        // create ei context
        let context = ei::Context::new(stream)?;
        context.flush()?;

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
        let event_stream = EiConvertEventStream::new(event_stream);
        let producer = Self {
            context,
            // input_capture,
            // session,
            event_stream,
            zones,
        };

        Ok(producer)
    }
}

impl EventProducer for LibeiProducer {
    fn notify(&mut self, _event: ClientEvent) -> io::Result<()> {
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        // FIXME
        // self.input_capture.release(&self.session, 0, (1.,0.));
        Ok(())
    }
}

impl LibeiProducer {
    fn handle_libei_event(&mut self, event: EiEvent) -> Option<(ClientHandle, Event)> {
        // FIXME
        let client = 0;
        match event {
            EiEvent::SeatAdded(seat_event) => {
                seat_event.seat.bind_capabilities(&[
                    DeviceCapability::Pointer,
                    DeviceCapability::PointerAbsolute,
                    DeviceCapability::Keyboard,
                    DeviceCapability::Touch,
                    DeviceCapability::Scroll,
                    DeviceCapability::Button,
                ]);
                let _ = self.context.flush();
                None
            }
            EiEvent::SeatRemoved(_) => None,
            EiEvent::DeviceAdded(d) => None,
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
}

impl Stream for LibeiProducer {
    type Item = io::Result<(ClientHandle, Event)>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            let event = match ready!(self.event_stream.poll_next_unpin(cx)) {
                None => return Poll::Ready(None),
                Some(Err(e)) => match e {
                    reis::tokio::EiConvertEventStreamError::Io(e) => {
                        return Poll::Ready(Some(Err(e)))
                    }
                    reis::tokio::EiConvertEventStreamError::Parse(e) => {
                        panic!("parse error: {}", e)
                    }
                    reis::tokio::EiConvertEventStreamError::Event(e) => {
                        panic!("event error: {:?}", e)
                    }
                },
                Some(Ok(event)) => event,
            };
            if let Some(e) = self.handle_libei_event(event) {
                return Poll::Ready(Some(Ok(e)));
            }
        }
    }
}
