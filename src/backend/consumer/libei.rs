use std::{os::{fd::{RawFd, FromRawFd}, unix::net::UnixStream}, collections::HashMap, time::{Instant, Duration, UNIX_EPOCH, SystemTime}};

use anyhow::{anyhow, Result};
use futures::StreamExt;
use ashpd::desktop::remote_desktop::RemoteDesktop;
use async_trait::async_trait;

use reis::{ei::{self, handshake::ContextType}, tokio::EiEventStream, PendingRequestResult, Object};

use crate::{consumer::AsyncConsumer, event::Event, client::{ClientHandle, ClientEvent}};

pub struct LibeiConsumer {
    handshake: bool,
    context: ei::Context,
    events: EiEventStream,
    pointer: Option<ei::Pointer>,
    has_pointer: bool,
    button: Option<ei::Button>,
    has_button: bool,
    capabilities: HashMap<String, u64>,
    capability_mask: u64,
    sequence: u32,
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
        })
    }
}

#[async_trait]
impl AsyncConsumer for LibeiConsumer {
    async fn consume(&mut self, event: Event, client_handle: ClientHandle) {
        log::warn!("ignoring ({client_handle:?}, {event:?}) - not yet implemented")
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
                    ei::device::Event::Name { name } => {log::debug!("{name}")},
                    ei::device::Event::DeviceType { device_type } => log::debug!("{device_type:?}"),
                    ei::device::Event::Dimensions { width, height } => log::debug!("{width}x{height}"),
                    ei::device::Event::Region { offset_x, offset_y, width, hight, scale } => log::debug!("region: {width}x{hight} @ ({offset_x},{offset_y}), scale: {scale}"),
                    ei::device::Event::Interface { object } => {
                        log::debug!("OBJECT: {object:?}");
                        log::debug!("INTERFACE: {}", object.interface());
                        if object.interface().eq("ei_pointer") {
                            self.pointer.replace(object.downcast().unwrap());
                            self.has_pointer = true;
                        } else if object.interface().eq("ei_button") {
                            self.button.replace(object.downcast().unwrap());
                            self.has_button = true;
                        }
                    }
                    ei::device::Event::Done => { },
                    ei::device::Event::Resumed { serial } => {
                        log::debug!("resumed {serial}");
                        device.start_emulating(serial, self.sequence);
                        self.sequence += 1;
                        if let Some(p) = self.pointer.as_mut() {
                            if self.has_pointer {
                                p.motion_relative(100.0, 0.0);
                                device.frame(self.sequence, SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64);
                                self.sequence += 1;
                            }
                        }
                    }
                    ei::device::Event::Paused { serial: _ } => {
                        self.has_pointer = false;
                        self.has_button = false;
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
                    ei::seat::Event::Destroyed { serial:_ } => {
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

