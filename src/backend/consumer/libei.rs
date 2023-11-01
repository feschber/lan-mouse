use std::{os::{fd::{RawFd, FromRawFd}, unix::net::UnixStream}, collections::HashMap, time::{UNIX_EPOCH, SystemTime}};

use anyhow::{anyhow, Result};
use futures::StreamExt;
use ashpd::desktop::remote_desktop::RemoteDesktop;
use async_trait::async_trait;

use reis::{ei::{self, handshake::ContextType}, tokio::EiEventStream, PendingRequestResult};

use crate::{consumer::AsyncConsumer, event::Event, client::{ClientHandle, ClientEvent}};

pub struct LibeiConsumer {
    handshake: bool,
    context: ei::Context,
    events: EiEventStream,
    pointer: Option<(ei::Device, ei::Pointer)>,
    has_pointer: bool,
    button: Option<ei::Button>,
    has_button: bool,
    capabilities: HashMap<String, u64>,
    capability_mask: u64,
    sequence: u32,
    serial: u32,
}

async fn get_ei_fd() -> Result<RawFd, ashpd::Error> {
    let proxy = RemoteDesktop::new().await?;
    let session = proxy.create_session().await?;
    proxy.start(&session, &ashpd::WindowIdentifier::default()).await?.response()?;
    proxy.connect_to_eis(&session).await
}

impl LibeiConsumer {
    pub async fn new() -> Result<Self> {
        let eifd = get_ei_fd().await?;
        let stream = unsafe { UnixStream::from_raw_fd(eifd) };
        stream.set_nonblocking(true)?;
        let context = ei::Context::new(stream)?;
        context.flush()?;
        let events = EiEventStream::new(context.clone())?;
        return Ok(Self {
            handshake: false,
            context, events,
            pointer: None, button: None,
            has_pointer: false,
            has_button: false,
            capabilities: HashMap::new(),
            capability_mask: 0,
            sequence: 0,
            serial: 0,
        })
    }
}

#[async_trait]
impl AsyncConsumer for LibeiConsumer {
    async fn consume(&mut self, event: Event, _client_handle: ClientHandle) {
        match event {
            Event::Pointer(p) => match p {
                crate::event::PointerEvent::Motion { time:_, relative_x, relative_y } => {
                    if self.has_pointer {
                        if let Some((d, p)) = self.pointer.as_mut() {
                            p.motion_relative(relative_x as f32, relative_y as f32);
                            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64;
                            d.frame(self.serial, now);
                        }
                    }
                },
                crate::event::PointerEvent::Button { time: _, button: _, state: _ } => {},
                crate::event::PointerEvent::Axis { time: _, axis: _, value: _ } => {},
                crate::event::PointerEvent::Frame {  } => {},
            },
            Event::Keyboard(_) => {},
            Event::Release() => {},
            Event::Ping() => {},
            Event::Pong() => {},
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
            PendingRequestResult::ProtocolError(e) => return Err(anyhow!("libei protocol violation: {e}")),
            PendingRequestResult::InvalidObject(e) => return Err(anyhow!("invalid object {e}")),
        };
        log::debug!("{event:?}");
        if !self.handshake {
            match event {
                ei::Event::Handshake(handshake, request) => match request {
                    ei::handshake::Event::HandshakeVersion { version } => {
                        log::info!("libei version {}", version);
                        // sender means we are sending events _to_ the eis server
                        handshake.handshake_version(version); // FIXME
                        handshake.context_type(ContextType::Sender);
                        handshake.name("lan-mouse");
                        handshake.interface_version("ei_connection", 1);
                        // handshake.interface_version("ei_handshake", 1);
                        handshake.interface_version("ei_callback", 1);
                        handshake.interface_version("ei_pingpong", 1);
                        handshake.interface_version("ei_seat", 1);
                        handshake.interface_version("ei_device", 2);
                        handshake.interface_version("ei_pointer", 1);
                        // handshake.interface_version("ei_pointer_absolute", 1);
                        // handshake.interface_version("ei_scroll", 1);
                        // handshake.interface_version("ei_button", 1);
                        // handshake.interface_version("ei_keyboard", 1);
                        // handshake.interface_version("ei_touchscreen", 1);
                        handshake.finish();
                        self.handshake = true;
                    }
                    r => log::debug!("{r:?}"),
                }
                _ => return Ok(()),
            }
        } else {
            match event {
                ei::Event::Connection(_connection, request) => match request {
                    ei::connection::Event::Seat { seat } => {
                        log::debug!("{seat:?}");
                    }
                    ei::connection::Event::Ping { ping } => {
                        ping.done(0);
                    }
                    ei::connection::Event::Disconnected { last_serial: _, reason, explanation } => {
                        log::debug!("ei - disconnected: reason: {reason:?}: {explanation}")
                    }
                    _ => {}
                }
                ei::Event::Device(device, request) => match request {
                    ei::device::Event::Destroyed { serial } => { log::debug!("destroyed {serial}") },
                    ei::device::Event::Name { name } => {log::debug!("device name: {name}")},
                    ei::device::Event::DeviceType { device_type } => log::debug!("{device_type:?}"),
                    ei::device::Event::Dimensions { width, height } => log::debug!("{width}x{height}"),
                    ei::device::Event::Region { offset_x, offset_y, width, hight, scale } => log::debug!("region: {width}x{hight} @ ({offset_x},{offset_y}), scale: {scale}"),
                    ei::device::Event::Interface { object } => {
                        log::debug!("OBJECT: {object:?}");
                        log::debug!("INTERFACE: {}", object.interface());
                        if object.interface().eq("ei_pointer") {
                            self.pointer.replace((device, object.downcast().unwrap()));
                        } else if object.interface().eq("ei_button") {
                            self.button.replace(object.downcast().unwrap());
                        }
                    }
                    ei::device::Event::Done => { },
                    ei::device::Event::Resumed { serial } => {
                        self.serial = serial;
                        if let Some((d,_)) = &mut self.pointer {
                            if d == &device {
                                log::debug!("pointer resumed {serial}");
                                self.sequence += 1;
                                d.start_emulating(serial, self.sequence);
                                self.has_pointer = true;
                            }
                        }
                    }
                    ei::device::Event::Paused { serial } => {
                        self.has_pointer = false;
                        self.has_button = false;
                        self.serial = serial;
                    },
                    ei::device::Event::StartEmulating { serial, sequence } => log::debug!("start emulating {serial}, {sequence}"),
                    ei::device::Event::StopEmulating { serial } => log::debug!("stop emulating {serial}"),
                    ei::device::Event::Frame { serial, timestamp } => {
                        log::debug!("frame: {serial}, {timestamp}");
                    }
                    ei::device::Event::RegionMappingId { mapping_id } => log::debug!("RegionMappingId {mapping_id}"),
                    e => log::debug!("invalid event: {e:?}"),
                }
                ei::Event::Seat(seat, request) => match request {
                    ei::seat::Event::Destroyed { serial } => {
                        self.serial = serial;
                        log::debug!("seat destroyed");
                    },
                    ei::seat::Event::Name { name } => {
                        log::debug!("connected to seat {name}");
                    },
                    ei::seat::Event::Capability { mask, interface } => {
                        self.capabilities.insert(interface, mask);
                        self.capability_mask |= mask;
                    },
                    ei::seat::Event::Done => {
                        seat.bind(self.capability_mask);
                    },
                    ei::seat::Event::Device { device } => {
                        log::debug!("new device: {device:?}");
                    },
                    _ => todo!(),
                }
                e => log::debug!("{e:?}"),
            }
        }
        self.context.flush()?;
        Ok(())
    }

    async fn notify(&mut self, _client_event: ClientEvent) {}

    async fn destroy(&mut self) {}
}

