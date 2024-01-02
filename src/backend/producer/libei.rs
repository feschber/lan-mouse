use anyhow::Result;
use ashpd::desktop::remote_desktop::RemoteDesktop;
use futures::StreamExt;
use reis::{ei, tokio::EiEventStream};
use std::{collections::HashMap, error::Error, io, os::{unix::net::UnixStream, fd::{FromRawFd, RawFd}}, task::{ready, Context, Poll}, pin::Pin};

use futures_core::Stream;

use crate::{
    client::{ClientEvent, ClientHandle},
    event::Event,
    producer::EventProducer,
};

#[allow(dead_code)]
pub struct LibeiProducer {
    handshake: bool,
    context: ei::Context,
    events: EiEventStream,
    has_pointer: bool,
    pointer: Option<(ei::Device, ei::Pointer)>,
    has_scroll: bool,
    scroll: Option<(ei::Device, ei::Scroll)>,
    has_keyboard: bool,
    button: Option<(ei::Device, ei::Button)>,
    has_button: bool,
    keyboard: Option<(ei::Device, ei::Keyboard)>,
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

impl LibeiProducer {
    pub async fn new() -> Result<Self, Box<dyn Error>> {
        let eifd = get_ei_fd().await?;
        let stream = unsafe { UnixStream::from_raw_fd(eifd) };
        stream.set_nonblocking(true)?;
        let context = ei::Context::new(stream)?;
        context.flush()?;
        let events = EiEventStream::new(context.clone())?;
        return Ok(Self {
            handshake: false,
            context, events,
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

impl EventProducer for LibeiProducer {
    fn notify(&mut self, _event: ClientEvent) -> io::Result<()> {
        Ok(())
    }

    fn release(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl LibeiProducer {

    fn handle_libei_event(&mut self, event: ei::Event) -> Option<(ClientHandle, Event)> {
        match event {
            ei::Event::Handshake(_, _) => None,
            ei::Event::Connection(_, _) => None,
            ei::Event::Callback(_, _) => None,
            ei::Event::Pingpong(_, _) => None,
            ei::Event::Seat(_, _) => None,
            ei::Event::Device(_, _) => None,
            ei::Event::Pointer(_, _) => None,
            ei::Event::PointerAbsolute(_, _) => None,
            ei::Event::Scroll(_, _) => None,
            ei::Event::Button(_, _) => None,
            ei::Event::Keyboard(_, _) => None,
            ei::Event::Touchscreen(_, _) => None,
            _ => todo!(),
        }
    }
}

impl Stream for LibeiProducer {
    type Item = io::Result<(ClientHandle, Event)>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>
    ) -> Poll<Option<Self::Item>> {
        loop {
            let event = match ready!(self.events.poll_next_unpin(cx)) {
                None => return Poll::Ready(None),
                Some(Err(e)) => return Poll::Ready(Some(Err(e))),
                Some(Ok(event)) => event,
            };
            let event = match event {
                reis::PendingRequestResult::ParseError(e) => {
                    log::warn!("libei protocol error: {e}");
                    return Poll::Ready(None)
                }
                reis::PendingRequestResult::InvalidObject(o) => {
                    log::warn!("libei invalid object: {o}");
                    return Poll::Ready(None)
                }
                reis::PendingRequestResult::Request(r) => r,
            };
            if let Some(e) = self.handle_libei_event(event) {
                return Poll::Ready(Some(Ok(e)));
            }
        }
    }
}
