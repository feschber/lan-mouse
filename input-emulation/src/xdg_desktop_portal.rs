use ashpd::{
    desktop::{
        remote_desktop::{Axis, DeviceType, KeyState, RemoteDesktop},
        PersistMode, Session,
    },
    zbus::AsyncDrop,
};
use async_trait::async_trait;

use futures::FutureExt;
use input_event::{
    Event::{Keyboard, Pointer},
    KeyboardEvent, PointerEvent,
};

use crate::error::EmulationError;

use super::{error::XdpEmulationCreationError, Emulation, EmulationHandle};

pub(crate) struct DesktopPortalEmulation<'a> {
    proxy: RemoteDesktop<'a>,
    session: Session<'a, RemoteDesktop<'a>>,
}

impl<'a> DesktopPortalEmulation<'a> {
    pub(crate) async fn new() -> Result<DesktopPortalEmulation<'a>, XdpEmulationCreationError> {
        log::debug!("connecting to org.freedesktop.portal.RemoteDesktop portal ...");
        let proxy = RemoteDesktop::new().await?;

        // retry when user presses the cancel button
        log::debug!("creating session ...");
        let session = proxy.create_session().await?;

        log::debug!("selecting devices ...");
        proxy
            .select_devices(
                &session,
                DeviceType::Keyboard | DeviceType::Pointer,
                None,
                PersistMode::ExplicitlyRevoked,
            )
            .await?;

        log::info!("requesting permission for input emulation");
        let _devices = proxy.start(&session, None).await?.response()?;

        log::debug!("started session");
        let session = session;

        Ok(Self { proxy, session })
    }
}

#[async_trait]
impl<'a> Emulation for DesktopPortalEmulation<'a> {
    async fn consume(
        &mut self,
        event: input_event::Event,
        _client: EmulationHandle,
    ) -> Result<(), EmulationError> {
        match event {
            Pointer(p) => match p {
                PointerEvent::Motion { time: _, dx, dy } => {
                    self.proxy
                        .notify_pointer_motion(&self.session, dx, dy)
                        .await?;
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
                    self.proxy
                        .notify_pointer_button(&self.session, button as i32, state)
                        .await?;
                }
                PointerEvent::AxisDiscrete120 { axis, value } => {
                    let axis = match axis {
                        0 => Axis::Vertical,
                        _ => Axis::Horizontal,
                    };
                    self.proxy
                        .notify_pointer_axis_discrete(&self.session, axis, value / 120)
                        .await?;
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
                    self.proxy
                        .notify_pointer_axis(&self.session, dx, dy, true)
                        .await?;
                }
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
                        self.proxy
                            .notify_keyboard_keycode(&self.session, key as i32, state)
                            .await?;
                    }
                    KeyboardEvent::Modifiers { .. } => {
                        // ignore
                    }
                }
            }
        }
        Ok(())
    }

    async fn create(&mut self, _client: EmulationHandle) {}
    async fn destroy(&mut self, _client: EmulationHandle) {}
    async fn terminate(&mut self) {
        if let Err(e) = self.session.close().await {
            log::warn!("session.close(): {e}");
        };
        if let Err(e) = self.session.receive_closed().await {
            log::warn!("session.receive_closed(): {e}");
        };
    }
}

impl<'a> AsyncDrop for DesktopPortalEmulation<'a> {
    #[doc = r" Perform the async cleanup."]
    #[must_use]
    #[allow(clippy::type_complexity, clippy::type_repetition_in_bounds)]
    fn async_drop<'async_trait>(
        self,
    ) -> ::core::pin::Pin<
        Box<dyn ::core::future::Future<Output = ()> + ::core::marker::Send + 'async_trait>,
    >
    where
        Self: 'async_trait,
    {
        async move {
            let _ = self.session.close().await;
        }
        .boxed()
    }
}
