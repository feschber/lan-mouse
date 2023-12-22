use std::{
    collections::HashMap,
    io,
    os::{
        fd::{FromRawFd, RawFd},
        unix::net::UnixStream,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Result};
use ashpd::desktop::remote_desktop::{DeviceType, RemoteDesktop};
use async_trait::async_trait;
use futures::StreamExt;

use reis::{
    ei::{self, button::ButtonState, handshake::ContextType, keyboard::KeyState},
    tokio::EiEventStream,
    PendingRequestResult,
};

use crate::{
    client::{ClientEvent, ClientHandle},
    consumer::EventConsumer,
    event::Event,
};

pub struct LibeiConsumer {
    handshake: bool,
    context: ei::Context,
    events: EiEventStream,
    pointer: Option<(ei::Device, ei::Pointer)>,
    has_pointer: bool,
    scroll: Option<(ei::Device, ei::Scroll)>,
    has_scroll: bool,
    button: Option<(ei::Device, ei::Button)>,
    has_button: bool,
    keyboard: Option<(ei::Device, ei::Keyboard)>,
    has_keyboard: bool,
    capabilities: HashMap<String, u64>,
    capability_mask: u64,
    sequence: u32,
    serial: u32,
}

async fn get_ei_fd() -> Result<RawFd, ashpd::Error> {
    let proxy = RemoteDesktop::new().await?;
    let session = proxy.create_session().await?;

    // I HATE EVERYTHING, THIS TOOK 8 HOURS OF DEBUGGING
    proxy
        .select_devices(
            &session,
            DeviceType::Pointer | DeviceType::Keyboard | DeviceType::Touchscreen,
        )
        .await?;

    proxy
        .start(&session, &ashpd::WindowIdentifier::default())
        .await?
        .response()?;
    proxy.connect_to_eis(&session).await
}

impl LibeiConsumer {
    pub async fn new() -> Result<Self> {
        // fd is owned by the message, so we need to dup it
        let eifd = get_ei_fd().await?;
        let eifd = unsafe {
            let ret = libc::dup(eifd);
            if ret < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(ret)
            }
        }?;
        let stream = unsafe { UnixStream::from_raw_fd(eifd) };
        // let stream = UnixStream::connect("/run/user/1000/eis-0")?;
        stream.set_nonblocking(true)?;
        let context = ei::Context::new(stream)?;
        context.flush()?;
        let events = EiEventStream::new(context.clone())?;
        Ok(Self {
            handshake: false,
            context,
            events,
            pointer: None,
            button: None,
            scroll: None,
            keyboard: None,
            has_pointer: false,
            has_button: false,
            has_scroll: false,
            has_keyboard: false,
            capabilities: HashMap::new(),
            capability_mask: 0,
            sequence: 0,
            serial: 0,
        })
    }
}

#[async_trait]
impl EventConsumer for LibeiConsumer {
    async fn consume(&mut self, event: Event, _client_handle: ClientHandle) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
        match event {
            Event::Pointer(p) => match p {
                crate::event::PointerEvent::Motion {
                    time: _,
                    relative_x,
                    relative_y,
                } => {
                    if !self.has_pointer {
                        return;
                    }
                    if let Some((d, p)) = self.pointer.as_mut() {
                        p.motion_relative(relative_x as f32, relative_y as f32);
                        d.frame(self.serial, now);
                    }
                }
                crate::event::PointerEvent::Button {
                    time: _,
                    button,
                    state,
                } => {
                    if !self.has_button {
                        return;
                    }
                    if let Some((d, b)) = self.button.as_mut() {
                        b.button(
                            button,
                            match state {
                                0 => ButtonState::Released,
                                _ => ButtonState::Press,
                            },
                        );
                        d.frame(self.serial, now);
                    }
                }
                crate::event::PointerEvent::Axis {
                    time: _,
                    axis,
                    value,
                } => {
                    if !self.has_scroll {
                        return;
                    }
                    if let Some((d, s)) = self.scroll.as_mut() {
                        match axis {
                            0 => s.scroll(0., value as f32),
                            _ => s.scroll(value as f32, 0.),
                        }
                        d.frame(self.serial, now);
                    }
                }
                crate::event::PointerEvent::Frame {} => {}
            },
            Event::Keyboard(k) => match k {
                crate::event::KeyboardEvent::Key {
                    time: _,
                    key,
                    state,
                } => {
                    if !self.has_keyboard {
                        return;
                    }
                    if let Some((d, k)) = &mut self.keyboard {
                        k.key(
                            key,
                            match state {
                                0 => KeyState::Released,
                                _ => KeyState::Press,
                            },
                        );
                        d.frame(self.serial, now);
                    }
                }
                crate::event::KeyboardEvent::Modifiers { .. } => {}
            },
            _ => {}
        }
        self.context.flush().unwrap();
    }

    async fn dispatch(&mut self) -> Result<()> {
        let event = match self.events.next().await {
            Some(e) => e?,
            None => return Err(anyhow!("libei connection lost")),
        };
        let event = match event {
            PendingRequestResult::Request(result) => result,
            PendingRequestResult::ParseError(e) => {
                return Err(anyhow!("libei protocol violation: {e}"))
            }
            PendingRequestResult::InvalidObject(e) => return Err(anyhow!("invalid object {e}")),
        };
        match event {
            ei::Event::Handshake(handshake, request) => match request {
                ei::handshake::Event::HandshakeVersion { version } => {
                    if self.handshake {
                        return Ok(());
                    }
                    log::info!("libei version {}", version);
                    // sender means we are sending events _to_ the eis server
                    handshake.handshake_version(version); // FIXME
                    handshake.context_type(ContextType::Sender);
                    handshake.name("ei-demo-client");
                    handshake.interface_version("ei_connection", 1);
                    handshake.interface_version("ei_callback", 1);
                    handshake.interface_version("ei_pingpong", 1);
                    handshake.interface_version("ei_seat", 1);
                    handshake.interface_version("ei_device", 2);
                    handshake.interface_version("ei_pointer", 1);
                    handshake.interface_version("ei_pointer_absolute", 1);
                    handshake.interface_version("ei_scroll", 1);
                    handshake.interface_version("ei_button", 1);
                    handshake.interface_version("ei_keyboard", 1);
                    handshake.interface_version("ei_touchscreen", 1);
                    handshake.finish();
                    self.handshake = true;
                }
                ei::handshake::Event::InterfaceVersion { name, version } => {
                    log::debug!("handshake: Interface {name} @ {version}");
                }
                ei::handshake::Event::Connection { serial, connection } => {
                    connection.sync(1);
                    self.serial = serial;
                }
                _ => unreachable!(),
            },
            ei::Event::Connection(_connection, request) => match request {
                ei::connection::Event::Seat { seat } => {
                    log::debug!("connected to seat: {seat:?}");
                }
                ei::connection::Event::Ping { ping } => {
                    ping.done(0);
                }
                ei::connection::Event::Disconnected {
                    last_serial: _,
                    reason,
                    explanation,
                } => {
                    log::debug!("ei - disconnected: reason: {reason:?}: {explanation}")
                }
                ei::connection::Event::InvalidObject {
                    last_serial,
                    invalid_id,
                } => {
                    return Err(anyhow!(
                        "invalid object: id: {invalid_id}, serial: {last_serial}"
                    ));
                }
                _ => unreachable!(),
            },
            ei::Event::Device(device, request) => match request {
                ei::device::Event::Destroyed { serial } => {
                    log::debug!("device destroyed: {device:?} - serial: {serial}")
                }
                ei::device::Event::Name { name } => {
                    log::debug!("device name: {name}")
                }
                ei::device::Event::DeviceType { device_type } => {
                    log::debug!("device type: {device_type:?}")
                }
                ei::device::Event::Dimensions { width, height } => {
                    log::debug!("device dimensions: {width}x{height}")
                }
                ei::device::Event::Region {
                    offset_x,
                    offset_y,
                    width,
                    hight,
                    scale,
                } => log::debug!(
                    "device region: {width}x{hight} @ ({offset_x},{offset_y}), scale: {scale}"
                ),
                ei::device::Event::Interface { object } => {
                    log::debug!("device interface: {object:?}");
                    if object.interface().eq("ei_pointer") {
                        log::debug!("GOT POINTER DEVICE");
                        self.pointer.replace((device, object.downcast().unwrap()));
                    } else if object.interface().eq("ei_button") {
                        log::debug!("GOT BUTTON DEVICE");
                        self.button.replace((device, object.downcast().unwrap()));
                    } else if object.interface().eq("ei_scroll") {
                        log::debug!("GOT SCROLL DEVICE");
                        self.scroll.replace((device, object.downcast().unwrap()));
                    } else if object.interface().eq("ei_keyboard") {
                        log::debug!("GOT KEYBOARD DEVICE");
                        self.keyboard.replace((device, object.downcast().unwrap()));
                    }
                }
                ei::device::Event::Done => {
                    log::debug!("device: done {device:?}");
                }
                ei::device::Event::Resumed { serial } => {
                    self.serial = serial;
                    device.start_emulating(serial, self.sequence);
                    self.sequence += 1;
                    log::debug!("resumed: {device:?}");
                    if let Some((d, _)) = &mut self.pointer {
                        if d == &device {
                            log::debug!("pointer resumed {serial}");
                            self.has_pointer = true;
                        }
                    }
                    if let Some((d, _)) = &mut self.button {
                        if d == &device {
                            log::debug!("button resumed {serial}");
                            self.has_button = true;
                        }
                    }
                    if let Some((d, _)) = &mut self.scroll {
                        if d == &device {
                            log::debug!("scroll resumed {serial}");
                            self.has_scroll = true;
                        }
                    }
                    if let Some((d, _)) = &mut self.keyboard {
                        if d == &device {
                            log::debug!("keyboard resumed {serial}");
                            self.has_keyboard = true;
                        }
                    }
                }
                ei::device::Event::Paused { serial } => {
                    self.has_pointer = false;
                    self.has_button = false;
                    self.serial = serial;
                }
                ei::device::Event::StartEmulating { serial, sequence } => {
                    log::debug!("start emulating {serial}, {sequence}")
                }
                ei::device::Event::StopEmulating { serial } => {
                    log::debug!("stop emulating {serial}")
                }
                ei::device::Event::Frame { serial, timestamp } => {
                    log::debug!("frame: {serial}, {timestamp}");
                }
                ei::device::Event::RegionMappingId { mapping_id } => {
                    log::debug!("RegionMappingId {mapping_id}")
                }
                e => log::debug!("invalid event: {e:?}"),
            },
            ei::Event::Seat(seat, request) => match request {
                ei::seat::Event::Destroyed { serial } => {
                    self.serial = serial;
                    log::debug!("seat destroyed: {seat:?}");
                }
                ei::seat::Event::Name { name } => {
                    log::debug!("seat name: {name}");
                }
                ei::seat::Event::Capability { mask, interface } => {
                    log::debug!("seat capabilities: {mask}, interface: {interface:?}");
                    self.capabilities.insert(interface, mask);
                    self.capability_mask |= mask;
                }
                ei::seat::Event::Done => {
                    log::debug!("seat done");
                    log::debug!("binding capabilities: {}", self.capability_mask);
                    seat.bind(self.capability_mask);
                }
                ei::seat::Event::Device { device } => {
                    log::debug!("seat: new device - {device:?}");
                }
                _ => todo!(),
            },
            e => log::debug!("unhandled event: {e:?}"),
        }
        self.context.flush()?;
        Ok(())
    }

    async fn notify(&mut self, _client_event: ClientEvent) {}

    async fn destroy(&mut self) {}
}
