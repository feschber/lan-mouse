use anyhow::Result;
use ashpd::{
    desktop::{
        remote_desktop::{Axis, DeviceType, KeyState, RemoteDesktop},
        Session,
    },
    WindowIdentifier,
};
use async_trait::async_trait;

use crate::{
    client::ClientEvent,
    consumer::EventConsumer,
    event::{
        Event::{Keyboard, Pointer},
        KeyboardEvent, PointerEvent,
    },
};

pub struct DesktopPortalConsumer<'a> {
    proxy: RemoteDesktop<'a>,
    session: Session<'a>,
}

impl<'a> DesktopPortalConsumer<'a> {
    pub async fn new() -> Result<DesktopPortalConsumer<'a>> {
        log::debug!("connecting to org.freedesktop.portal.RemoteDesktop portal ...");
        let proxy = RemoteDesktop::new().await?;
        log::debug!("creating session ...");
        let session = proxy.create_session().await?;
        log::debug!("selecting devices ...");
        proxy
            .select_devices(&session, DeviceType::Keyboard | DeviceType::Pointer)
            .await?;

        let _ = proxy
            .start(&session, &WindowIdentifier::default())
            .await?
            .response()?;
        log::debug!("started session");

        Ok(Self { proxy, session })
    }
}

#[async_trait]
impl<'a> EventConsumer for DesktopPortalConsumer<'a> {
    async fn consume(&mut self, event: crate::event::Event, _client: crate::client::ClientHandle) {
        match event {
            Pointer(p) => {
                match p {
                    PointerEvent::Motion {
                        time: _,
                        relative_x,
                        relative_y,
                    } => {
                        if let Err(e) = self
                            .proxy
                            .notify_pointer_motion(&self.session, relative_x, relative_y)
                            .await
                        {
                            log::warn!("{e}");
                        }
                    }
                    PointerEvent::Button {
                        time: _,
                        button,
                        state,
                    } => {
                        let state = match state {
                            0 => KeyState::Released,
                            _ => KeyState::Pressed,
                        };
                        if let Err(e) = self
                            .proxy
                            .notify_pointer_button(&self.session, button as i32, state)
                            .await
                        {
                            log::warn!("{e}");
                        }
                    }
                    PointerEvent::Axis {
                        time: _,
                        axis,
                        value,
                    } => {
                        let axis = match axis {
                            0 => Axis::Vertical,
                            _ => Axis::Horizontal,
                        };
                        // TODO smooth scrolling
                        if let Err(e) = self
                            .proxy
                            .notify_pointer_axis_discrete(&self.session, axis, value as i32)
                            .await
                        {
                            log::warn!("{e}");
                        }
                    }
                    PointerEvent::Frame {} => {}
                }
            }
            Keyboard(k) => {
                match k {
                    KeyboardEvent::Key {
                        time: _,
                        key,
                        state,
                    } => {
                        let state = match state {
                            0 => KeyState::Released,
                            _ => KeyState::Pressed,
                        };
                        if let Err(e) = self
                            .proxy
                            .notify_keyboard_keycode(&self.session, key as i32, state)
                            .await
                        {
                            log::warn!("{e}");
                        }
                    }
                    KeyboardEvent::Modifiers { .. } => {
                        // ignore
                    }
                }
            }
            _ => {}
        }
    }

    async fn notify(&mut self, _client: ClientEvent) {}

    async fn destroy(&mut self) {
        log::debug!("closing remote desktop session");
        if let Err(e) = self.session.close().await {
            log::error!("failed to close remote desktop session: {e}");
        }
    }
}
