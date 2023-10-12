use async_trait::async_trait;
use anyhow::Result;
use ashpd::{desktop::remote_desktop::{RemoteDesktop, DeviceType, KeyState}, WindowIdentifier, enumflags2::BitFlags};

use crate::consumer::AsyncConsumer;

pub struct DesktopPortalConsumer<'a> {
    devices: BitFlags<DeviceType>,
    proxy: RemoteDesktop<'a>,
    session: ashpd::desktop::Session<'a>,
}

impl<'a> DesktopPortalConsumer<'a> {
    pub async fn new() -> Result<DesktopPortalConsumer<'a>> {
        let proxy = RemoteDesktop::new().await?;
        let session = proxy.create_session().await?;
        proxy
            .select_devices(&session, DeviceType::Keyboard | DeviceType::Pointer)
            .await?;

        let response = proxy
            .start(&session, &WindowIdentifier::default())
            .await?
            .response()?;

        let devices = response.devices();

        Ok(Self { devices, proxy, session })
    }
}

#[async_trait]
impl<'a> AsyncConsumer for DesktopPortalConsumer<'a> {
    async fn consume(&mut self, event: crate::event::Event, _client: crate::client::ClientHandle) {
        match event {
            crate::event::Event::Pointer(p) => {
                match p {
                    crate::event::PointerEvent::Motion { time: _, relative_x, relative_y } => {
                        if let Err(e) = self.proxy.notify_pointer_motion(&self.session, relative_x, relative_y).await {
                            log::warn!("{e}");
                        }
                    },
                    crate::event::PointerEvent::Button { time: _, button, state } => {
                        let state = match state {
                            0 => KeyState::Released,
                            _ => KeyState::Pressed,
                        };
                        if let Err(e) = self.proxy.notify_pointer_button(&self.session, button as i32, state).await {
                            log::warn!("{e}");
                        }
                    },
                    crate::event::PointerEvent::Axis { time: _, axis, value } => {
                        let (dx, dy) = match axis {
                            0 => (value, 0.),
                            1 => (0., value),
                            _ => panic!("invalid axis"),
                        };
                        // TODO finished
                        // TODO smooth scrolling
                        if let Err(e) = self.proxy.notify_pointer_axis(&self.session, dx, dy, true).await {
                            log::warn!("{e}");
                        }

                    },
                    crate::event::PointerEvent::Frame {  } => {},
                }
            },
            crate::event::Event::Keyboard(k) => {
                match k {
                    crate::event::KeyboardEvent::Key { time: _, key, state } => {
                        let state = match state {
                            0 => KeyState::Released,
                            _ => KeyState::Pressed,
                        };
                        if let Err(e) = self.proxy.notify_keyboard_keycode(&self.session, key as i32, state).await {
                            log::warn!("{e}");
                        }
                    },
                    crate::event::KeyboardEvent::Modifiers { .. } => {
                        // ignore
                    },
                }
            },
            _ => {},
        }
    }

    async fn notify(&mut self, _client: crate::client::ClientEvent) {

    }
}
