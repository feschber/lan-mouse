use std::{
    future::{Future, poll_fn},
    task::{Context, Poll},
};

use ashpd::desktop::remote_desktop::{Axis, KeyState, NotifyPointerAxisOptions};
use async_trait::async_trait;
use input_event::{
    Event::{Keyboard, Pointer},
    KeyboardEvent, PointerEvent,
};

use crate::{error::EmulationError, portal::RemoteDesktopSession};

use super::{Emulation, EmulationHandle, error::XdpEmulationCreationError};

pub(crate) struct DesktopPortalEmulation {
    portal: RemoteDesktopSession,
}

impl DesktopPortalEmulation {
    pub(crate) async fn new() -> Result<DesktopPortalEmulation, XdpEmulationCreationError> {
        Ok(Self {
            portal: RemoteDesktopSession::new().await?,
        })
    }
}

#[async_trait]
impl Emulation for DesktopPortalEmulation {
    async fn consume(
        &mut self,
        event: input_event::Event,
        _client: EmulationHandle,
    ) -> Result<(), EmulationError> {
        let RemoteDesktopSession {
            proxy,
            session,
            closed,
            ..
        } = &mut self.portal;
        let notify = async {
            match event {
                Pointer(p) => match p {
                    PointerEvent::Motion { time: _, dx, dy } => {
                        proxy
                            .notify_pointer_motion(session, dx, dy, Default::default())
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
                        proxy
                            .notify_pointer_button(
                                session,
                                button as i32,
                                state,
                                Default::default(),
                            )
                            .await?;
                    }
                    PointerEvent::AxisDiscrete120 { axis, value } => {
                        let axis = match axis {
                            0 => Axis::Vertical,
                            _ => Axis::Horizontal,
                        };
                        proxy
                            .notify_pointer_axis_discrete(
                                session,
                                axis,
                                value / 120,
                                Default::default(),
                            )
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
                        proxy
                            .notify_pointer_axis(
                                session,
                                dx,
                                dy,
                                NotifyPointerAxisOptions::default().set_finish(true),
                            )
                            .await?;
                    }
                },
                Keyboard(k) => match k {
                    KeyboardEvent::Key {
                        time: _,
                        key,
                        state,
                    } => {
                        let state = match state {
                            0 => KeyState::Released,
                            _ => KeyState::Pressed,
                        };
                        proxy
                            .notify_keyboard_keycode(session, key as i32, state, Default::default())
                            .await?;
                    }
                    KeyboardEvent::Modifiers { .. } => {
                        // ignore
                    }
                },
            }
            Ok::<(), ashpd::Error>(())
        };
        futures::pin_mut!(notify);
        poll_fn(|cx| {
            // Prefer closure even when the notification reply is also ready.
            if closed.poll_closed(cx).is_ready() {
                return Poll::Ready(Err(EmulationError::EndOfStream));
            }
            notify
                .as_mut()
                .poll(cx)
                .map(|result| result.map_err(notify_error))
        })
        .await
    }

    async fn create(&mut self, _client: EmulationHandle) {}
    async fn destroy(&mut self, _client: EmulationHandle) {}

    fn poll_dispatch(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), EmulationError>> {
        self.portal
            .poll_closed(cx)
            .map(|()| Err(EmulationError::EndOfStream))
    }

    async fn terminate(&mut self) {
        self.portal.close().await;
    }
}

fn notify_error(error: ashpd::Error) -> EmulationError {
    use ashpd::{Error, PortalError};

    let invalid_session = match &error {
        Error::Portal(PortalError::Failed(message) | PortalError::InvalidArgument(message)) => {
            message == "Invalid session"
        }
        Error::Portal(PortalError::ZBus(error)) | Error::Zbus(error) => {
            matches!(
                error,
                ashpd::zbus::Error::MethodError(name, Some(message), _)
                    if name.as_str() == "org.freedesktop.zbus.Error"
                        && message == "Invalid session"
            )
        }
        _ => false,
    };
    if invalid_session {
        EmulationError::EndOfStream
    } else {
        EmulationError::Ashpd(error)
    }
}

#[cfg(test)]
mod tests {
    use ashpd::{Error, PortalError, desktop::ResponseError, zbus::Message};

    use super::notify_error;
    use crate::error::EmulationError;

    fn method_error(name: &str, message: &str) -> ashpd::zbus::Error {
        let call = Message::method_call("/org/freedesktop/portal/desktop", "NotifyPointerMotion")
            .unwrap()
            .build(&())
            .unwrap();
        Message::error(&call.header(), name)
            .unwrap()
            .build(&message)
            .unwrap()
            .into()
    }

    #[test]
    fn notify_error_recovers_observed_legacy_invalid_session() {
        let error = Error::Portal(PortalError::ZBus(method_error(
            "org.freedesktop.zbus.Error",
            "Invalid session",
        )));
        assert_eq!(
            error.to_string(),
            "Portal request failed: org.freedesktop.zbus.Error: Invalid session"
        );
        assert!(matches!(notify_error(error), EmulationError::EndOfStream));
    }

    #[test]
    fn notify_error_recovers_direct_zbus_invalid_session() {
        let error = Error::Zbus(method_error(
            "org.freedesktop.zbus.Error",
            "Invalid session",
        ));
        assert!(matches!(notify_error(error), EmulationError::EndOfStream));
    }

    #[test]
    fn notify_error_recovers_named_portal_invalid_session() {
        for error in [
            PortalError::Failed("Invalid session".into()),
            PortalError::InvalidArgument("Invalid session".into()),
        ] {
            assert!(matches!(
                notify_error(Error::Portal(error)),
                EmulationError::EndOfStream
            ));
        }
    }

    #[test]
    fn notify_error_preserves_permission_errors_even_with_invalid_session_body() {
        for error in [
            Error::Portal(PortalError::NotAllowed("Invalid session".into())),
            Error::Portal(PortalError::ZBus(method_error(
                "org.freedesktop.DBus.Error.AccessDenied",
                "Invalid session",
            ))),
        ] {
            assert!(matches!(notify_error(error), EmulationError::Ashpd(_)));
        }
    }

    #[test]
    fn notify_error_preserves_cancellation() {
        for error in [
            Error::Response(ResponseError::Cancelled),
            Error::Portal(PortalError::Cancelled("Invalid session".into())),
        ] {
            assert!(matches!(notify_error(error), EmulationError::Ashpd(_)));
        }
    }

    #[test]
    fn notify_error_preserves_unrelated_failures_and_original_details() {
        for error in [
            Error::Portal(PortalError::Failed("Permission denied".into())),
            Error::Portal(PortalError::ZBus(method_error(
                "org.freedesktop.zbus.Error",
                "Permission denied",
            ))),
            Error::Portal(PortalError::Failed(
                "Invalid session permission requested".into(),
            )),
            Error::NoResponse,
        ] {
            let expected = error.to_string();
            let EmulationError::Ashpd(actual) = notify_error(error) else {
                panic!("unrelated error was classified as session closure");
            };
            assert_eq!(actual.to_string(), expected);
        }
    }
}
