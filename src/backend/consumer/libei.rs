use std::os::{fd::{RawFd, FromRawFd}, unix::net::UnixStream};

use futures::StreamExt;
use anyhow::anyhow;
use ashpd::desktop::remote_desktop::RemoteDesktop;
use async_trait::async_trait;

use reis::{ei::{self, Handshake}, tokio::EiEventStream, PendingRequestResult};

use crate::{consumer::AsyncConsumer, event::Event, client::{ClientHandle, ClientEvent}};

pub struct LibeiConsumer {
    _context: ei::Context,
    _events: EiEventStream,
}

async fn get_ei_fd() -> Result<RawFd, ashpd::Error> {
    let proxy = RemoteDesktop::new().await?;
    let session = proxy.create_session().await?;
    proxy.start(&session, &ashpd::WindowIdentifier::default()).await?.response()?;
    proxy.connect_to_eis(&session).await
}

impl LibeiConsumer {
    pub async fn new() -> anyhow::Result<Self> {
        let eifd = get_ei_fd().await?;
        let stream = unsafe { UnixStream::from_raw_fd(eifd) };
        let context = ei::Context::new(stream)?;
        context.handshake();
        context.flush()?;
        let mut events = EiEventStream::new(context.clone())?;
        loop {
            let result = match events.next().await {
                Some(r) => r?,
                None => return Err(anyhow!("libei: connection closed unexpectedly")),
            };
            let request: ei::Event = match result {
                PendingRequestResult::Request(e) => e,
                PendingRequestResult::ProtocolError(msg) => {
                    return Err(anyhow!("libei protocol violation: {msg}"));
                }
                PendingRequestResult::InvalidObject(obj) => {
                    return Err(anyhow!("libei: invalid object ({obj})"));
                }
            };
            if let ei::Event::Handshake(handshake, request) = request {
                log::info!("handshake: {handshake:?}, request: {request:?}");
                return Ok(Self { _context: context, _events: events })
            }
        }

    }
}

#[async_trait]
impl AsyncConsumer for LibeiConsumer {
    async fn consume(&mut self, event: Event, client_handle: ClientHandle) {
        log::warn!("ignoring ({client_handle:?}, {event:?}) - not yet implemented")
    }
    async fn notify(&mut self, _client_event: ClientEvent) {}

    async fn destroy(&mut self) {}
}

