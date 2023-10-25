use std::{os::{fd::{RawFd, FromRawFd}, unix::net::UnixStream}, collections::HashMap};

use anyhow::{anyhow, Result};
use futures::StreamExt;
use ashpd::desktop::remote_desktop::RemoteDesktop;
use async_trait::async_trait;

use reis::{ei::{self, handshake::ContextType}, tokio::EiEventStream, PendingRequestResult, Object};

use crate::{consumer::AsyncConsumer, event::Event, client::{ClientHandle, ClientEvent}};

pub struct LibeiConsumer {
    context: ei::Context,
    events: EiEventStream,
    pointer: Option<ei::Pointer>,
    button: Option<ei::Button>,
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
        let handshake = context.handshake();
        log::debug!("{handshake:?}");
        context.flush()?;
        let events = EiEventStream::new(context.clone())?;
        return Ok(Self {
            context, events,
            pointer: None, button: None,
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
        match event {
            ei::Event::Handshake(handshake, request) => match request {
                ei::handshake::Event::HandshakeVersion { version } => {
                    log::info!("libei version {}", version);
                    // sender means we are sending events _to_ the eis server
                    handshake.handshake_version(version); // FIXME
                    handshake.context_type(ContextType::Sender);
                    handshake.name("lan-mouse");
                    handshake.interface_version("ei_callback", 1);
                    handshake.interface_version("ei_connection", 1);
                    handshake.interface_version("ei_pingpong", 1);
                    handshake.interface_version("ei_seat", 1);
                    handshake.interface_version("ei_device", 1);
                    handshake.interface_version("ei_pointer", 1);
                    handshake.finish();
                }
                r => log::debug!("{r:?}"),
            }
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
                    } else if object.interface().eq("ei_button") {
                        self.button.replace(object.downcast().unwrap());
                    }
                }
                ei::device::Event::Done => {
                    // device.start_emulating(0, 0);
                    // if let Some(p) = self.pointer.as_mut() {
                    //     p.motion_absolute(1.0, 0.0);
                    // } else {
                    //     panic!("no pointer");
                    // }
                },
                ei::device::Event::Resumed { serial } => {
                    log::debug!("resumed {serial}");
                    let button = self.button.as_mut().unwrap();
                    device.start_emulating(unsafe { std::mem::transmute::<&ei::Button,&reis::Object>(button) }.id() as u32, self.sequence);
                    self.sequence += 1;
                    if let Some(b) = self.button.as_mut() {
                        b.button(2, ei::button::ButtonState::Press);
                        b.button(2, ei::button::ButtonState::Released);
                    }
                }
                ei::device::Event::Paused { serial } => log::debug!("paused {serial}"),
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
        self.context.flush()?;
        Ok(())
    }

    async fn notify(&mut self, _client_event: ClientEvent) {}

    async fn destroy(&mut self) {}
}

