use anyhow::Result;
use ashpd::{
    desktop::{
        remote_desktop::{Axis, DeviceType, KeyState, RemoteDesktop},
        ResponseError, Session,
    },
    WindowIdentifier,
};
use async_trait::async_trait;

use crate::event::{
    Event::{Keyboard, Pointer},
    KeyboardEvent, PointerEvent,
};

use super::{error::XdpEmulationCreationError, EmulationHandle, InputEmulation};

pub struct DesktopPortalEmulation<'a> {
    proxy: RemoteDesktop<'a>,
    session: Option<Session<'a>>,
}

impl<'a> DesktopPortalEmulation<'a> {
    pub async fn new() -> Result<DesktopPortalEmulation<'a>, XdpEmulationCreationError> {
        log::debug!("connecting to org.freedesktop.portal.RemoteDesktop portal ...");
        let proxy = RemoteDesktop::new().await?;

        // retry when user presses the cancel button
        let (session, _) = loop {
            log::debug!("creating session ...");
            let session = proxy.create_session().await?;

            log::debug!("selecting devices ...");
            proxy
                .select_devices(&session, DeviceType::Keyboard | DeviceType::Pointer)
                .await?;

            log::info!("requesting permission for input emulation");
            match proxy
                .start(&session, &WindowIdentifier::default())
                .await?
                .response()
            {
                Ok(d) => break (session, d),
                Err(ashpd::Error::Response(ResponseError::Cancelled)) => {
                    log::warn!("request cancelled!");
                    continue;
                }
                e => e?,
            };
        };

        log::debug!("started session");
        let session = Some(session);

        Ok(Self { proxy, session })
    }
}

#[async_trait]
impl<'a> InputEmulation for DesktopPortalEmulation<'a> {
    async fn consume(&mut self, event: crate::event::Event, _client: crate::client::ClientHandle) {
        match event {
            Pointer(p) => match p {
                PointerEvent::Motion {
                    time: _,
                    relative_x,
                    relative_y,
                } => {
                    if let Err(e) = self
                        .proxy
                        .notify_pointer_motion(
                            self.session.as_ref().expect("no session"),
                            relative_x,
                            relative_y,
                        )
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
                        .notify_pointer_button(
                            self.session.as_ref().expect("no session"),
                            button as i32,
                            state,
                        )
                        .await
                    {
                        log::warn!("{e}");
                    }
                }
                PointerEvent::AxisDiscrete120 { axis, value } => {
                    let axis = match axis {
                        0 => Axis::Vertical,
                        _ => Axis::Horizontal,
                    };
                    if let Err(e) = self
                        .proxy
                        .notify_pointer_axis_discrete(
                            self.session.as_ref().expect("no session"),
                            axis,
                            value,
                        )
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
                    let (dx, dy) = match axis {
                        Axis::Vertical => (0., value),
                        Axis::Horizontal => (value, 0.),
                    };
                    if let Err(e) = self
                        .proxy
                        .notify_pointer_axis(
                            self.session.as_ref().expect("no session"),
                            dx,
                            dy,
                            true,
                        )
                        .await
                    {
                        log::warn!("{e}");
                    }
                }
                PointerEvent::Frame {} => {}
            },
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
                            .notify_keyboard_keycode(
                                self.session.as_ref().expect("no session"),
                                key as i32,
                                state,
                            )
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

    async fn create(&mut self, _client: EmulationHandle) {}
    async fn destroy(&mut self, _client: EmulationHandle) {}
}

impl<'a> Drop for DesktopPortalEmulation<'a> {
    fn drop(&mut self) {
        let session = self.session.take().expect("no session");
        tokio::runtime::Handle::try_current()
            .expect("no runtime")
            .block_on(async move {
                log::debug!("closing remote desktop session");
                if let Err(e) = session.close().await {
                    log::error!("failed to close remote desktop session: {e}");
                }
            });
    }
}
