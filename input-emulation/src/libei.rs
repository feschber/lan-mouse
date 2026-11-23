use futures::{StreamExt, task::AtomicWaker};
use std::{
    future::Future,
    io,
    os::unix::net::UnixStream,
    pin::Pin,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{Interest, unix::AsyncFd},
    task::JoinHandle,
};

use async_trait::async_trait;

use reis::{
    ei::{
        self, Button, Keyboard, Pointer, Scroll, button::ButtonState, handshake::ContextType,
        keyboard::KeyState,
    },
    event::{self, Connection, DeviceCapability, DeviceEvent, EiEvent, SeatEvent},
    tokio::EiConvertEventStream,
};

use input_event::{Event, KeyboardEvent, PointerEvent};

use crate::{error::EmulationError, portal::RemoteDesktopSession};

use super::{Emulation, EmulationHandle, error::LibeiEmulationCreationError};

#[derive(Clone, Default)]
struct Devices {
    pointer: Arc<RwLock<Option<(ei::Device, ei::Pointer)>>>,
    scroll: Arc<RwLock<Option<(ei::Device, ei::Scroll)>>>,
    button: Arc<RwLock<Option<(ei::Device, ei::Button)>>>,
    keyboard: Arc<RwLock<Option<(ei::Device, ei::Keyboard)>>>,
}

/// Flushes the shared reis queue without blocking input/control dispatch.
struct EiWriter {
    context: ei::Context,
    writable: AsyncFd<UnixStream>,
    pending: AtomicBool,
    waker: AtomicWaker,
}

impl EiWriter {
    fn new(context: ei::Context, stream: UnixStream) -> io::Result<Self> {
        Ok(Self {
            context,
            writable: AsyncFd::with_interest(stream, Interest::WRITABLE)?,
            pending: AtomicBool::new(false),
            waker: AtomicWaker::new(),
        })
    }

    fn flush(&self) -> Result<(), EmulationError> {
        if self.pending.load(Ordering::Acquire) {
            return Ok(());
        }
        match self.context.flush().map_err(io::Error::from) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                // reis retains the unsent suffix, including after a partial write.
                self.pending.store(true, Ordering::Release);
                self.waker.wake();
                Ok(())
            }
            Err(error) => Err(libei_io_error(error)),
        }
    }

    fn poll_flush(&self, cx: &mut Context<'_>) -> Poll<Result<(), EmulationError>> {
        // Device replies can queue writes while dispatch is otherwise idle.
        self.waker.register(cx.waker());
        if !self.pending.load(Ordering::Acquire) {
            return Poll::Pending;
        }
        loop {
            let mut ready = match self.writable.poll_write_ready(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(libei_io_error(error))),
                Poll::Ready(Ok(ready)) => ready,
            };
            if let Ok(result) = ready.try_io(|_| self.context.flush().map_err(io::Error::from)) {
                self.pending.store(false, Ordering::Release);
                return Poll::Ready(result.map_err(libei_io_error));
            }
        }
    }
}

pub(crate) struct LibeiEmulation {
    writer: Arc<EiWriter>,
    conn: event::Connection,
    devices: Devices,
    ei_task: JoinHandle<()>,
    error: Arc<Mutex<Option<EmulationError>>>,
    libei_error: Arc<AtomicBool>,
    portal: RemoteDesktopSession,
}

impl LibeiEmulation {
    pub(crate) async fn new() -> Result<Self, LibeiEmulationCreationError> {
        let portal = RemoteDesktopSession::new().await?;
        let eifd = portal
            .proxy
            .connect_to_eis(&portal.session, Default::default())
            .await?;
        let stream = UnixStream::from(eifd);
        stream.set_nonblocking(true)?;
        // reis registers the original fd for reads; use a duplicate for writes.
        let write_stream = stream.try_clone()?;
        let context = ei::Context::new(stream)?;
        let (conn, events) = context
            .handshake_tokio("de.feschber.LanMouse", ContextType::Sender)
            .await?;
        let writer = Arc::new(EiWriter::new(context, write_stream)?);
        let devices = Devices::default();
        let libei_error = Arc::new(AtomicBool::default());
        let error = Arc::new(Mutex::new(None));
        let ei_handler = ei_task(
            events,
            conn.clone(),
            writer.clone(),
            devices.clone(),
            libei_error.clone(),
            error.clone(),
        );
        let ei_task = tokio::task::spawn_local(ei_handler);

        Ok(Self {
            writer,
            conn,
            devices,
            ei_task,
            error,
            libei_error,
            portal,
        })
    }
}

impl Drop for LibeiEmulation {
    fn drop(&mut self) {
        self.ei_task.abort();
    }
}

#[async_trait]
impl Emulation for LibeiEmulation {
    fn poll_dispatch(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), EmulationError>> {
        if self.portal.poll_closed(cx).is_ready()
            || self.libei_error.load(Ordering::SeqCst)
            || Pin::new(&mut self.ei_task).poll(cx).is_ready()
        {
            self.libei_error.store(true, Ordering::SeqCst);
            Poll::Ready(Err(self
                .error
                .lock()
                .unwrap()
                .take()
                .unwrap_or(EmulationError::EndOfStream)))
        } else {
            self.writer.poll_flush(cx)
        }
    }

    async fn consume(
        &mut self,
        event: Event,
        _handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
        if self.libei_error.load(Ordering::SeqCst) {
            return Err(self
                .error
                .lock()
                .unwrap()
                .take()
                .unwrap_or(EmulationError::EndOfStream));
        }
        match event {
            Event::Pointer(p) => match p {
                PointerEvent::Motion { time: _, dx, dy } => {
                    let pointer_device = self.devices.pointer.read().unwrap();
                    if let Some((d, p)) = pointer_device.as_ref() {
                        p.motion_relative(dx as f32, dy as f32);
                        d.frame(self.conn.serial(), now);
                    }
                }
                PointerEvent::Button {
                    time: _,
                    button,
                    state,
                } => {
                    let button_device = self.devices.button.read().unwrap();
                    if let Some((d, b)) = button_device.as_ref() {
                        b.button(
                            button,
                            match state {
                                0 => ButtonState::Released,
                                _ => ButtonState::Press,
                            },
                        );
                        d.frame(self.conn.serial(), now);
                    }
                }
                PointerEvent::Axis {
                    time: _,
                    axis,
                    value,
                } => {
                    let scroll_device = self.devices.scroll.read().unwrap();
                    if let Some((d, s)) = scroll_device.as_ref() {
                        match axis {
                            0 => s.scroll(0., value as f32),
                            _ => s.scroll(value as f32, 0.),
                        }
                        d.frame(self.conn.serial(), now);
                    }
                }
                PointerEvent::AxisDiscrete120 { axis, value } => {
                    let scroll_device = self.devices.scroll.read().unwrap();
                    if let Some((d, s)) = scroll_device.as_ref() {
                        match axis {
                            0 => s.scroll_discrete(0, value),
                            _ => s.scroll_discrete(value, 0),
                        }
                        d.frame(self.conn.serial(), now);
                    }
                }
            },
            Event::Keyboard(k) => match k {
                KeyboardEvent::Key {
                    time: _,
                    key,
                    state,
                } => {
                    let keyboard_device = self.devices.keyboard.read().unwrap();
                    if let Some((d, k)) = keyboard_device.as_ref() {
                        k.key(
                            key,
                            match state {
                                0 => KeyState::Released,
                                _ => KeyState::Press,
                            },
                        );
                        d.frame(self.conn.serial(), now);
                    }
                }
                KeyboardEvent::Modifiers { .. } => {}
            },
        }
        self.writer.flush()
    }

    async fn create(&mut self, _: EmulationHandle) {}
    async fn destroy(&mut self, _: EmulationHandle) {}

    async fn terminate(&mut self) {
        self.portal.close().await;
        self.ei_task.abort();
    }
}

fn libei_io_error(error: io::Error) -> EmulationError {
    match error.kind() {
        // A compositor can reset EIS on lock instead of sending Disconnected.
        io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::NotConnected
        | io::ErrorKind::UnexpectedEof => {
            log::debug!("libei transport closed: {error}");
            EmulationError::EndOfStream
        }
        _ => error.into(),
    }
}

fn libei_stream_error(error: reis::Error) -> EmulationError {
    match error {
        reis::Error::Io(error) => libei_io_error(error),
        error => error.into(),
    }
}

async fn ei_task(
    mut events: EiConvertEventStream,
    _conn: Connection,
    writer: Arc<EiWriter>,
    devices: Devices,
    libei_error: Arc<AtomicBool>,
    error: Arc<Mutex<Option<EmulationError>>>,
) {
    if let Err(e) = ei_event_handler(&mut events, &writer, &devices).await {
        error.lock().unwrap().replace(e);
        libei_error.store(true, Ordering::SeqCst);
    }
}

async fn ei_event_handler(
    events: &mut EiConvertEventStream,
    writer: &EiWriter,
    devices: &Devices,
) -> Result<(), EmulationError> {
    loop {
        let event = events
            .next()
            .await
            .ok_or(EmulationError::EndOfStream)?
            .map_err(libei_stream_error)?;
        let capabilities = DeviceCapability::Pointer
            | DeviceCapability::PointerAbsolute
            | DeviceCapability::Keyboard
            | DeviceCapability::Touch
            | DeviceCapability::Scroll
            | DeviceCapability::Button;
        log::debug!("{event:?}");
        match event {
            EiEvent::Disconnected(e) => {
                log::debug!("ei disconnected: {e:?}");
                return Err(EmulationError::EndOfStream);
            }
            EiEvent::SeatAdded(e) => {
                e.seat().bind_capabilities(capabilities);
            }
            EiEvent::SeatRemoved(e) => {
                log::debug!("seat removed: {:?}", e.seat());
            }
            EiEvent::DeviceAdded(e) => {
                let device_type = e.device().device_type();
                log::debug!("device added: {device_type:?}");
                let device = e.device();
                if let Some(pointer) = e.device().interface::<Pointer>() {
                    devices
                        .pointer
                        .write()
                        .unwrap()
                        .replace((device.device().clone(), pointer));
                }
                if let Some(keyboard) = e.device().interface::<Keyboard>() {
                    devices
                        .keyboard
                        .write()
                        .unwrap()
                        .replace((device.device().clone(), keyboard));
                }
                if let Some(scroll) = e.device().interface::<Scroll>() {
                    devices
                        .scroll
                        .write()
                        .unwrap()
                        .replace((device.device().clone(), scroll));
                }
                if let Some(button) = e.device().interface::<Button>() {
                    devices
                        .button
                        .write()
                        .unwrap()
                        .replace((device.device().clone(), button));
                }
            }
            EiEvent::DeviceRemoved(e) => {
                log::debug!("device removed: {:?}", e.device().device_type());
            }
            EiEvent::DevicePaused(e) => {
                log::debug!("device paused: {:?}", e.device().device_type());
            }
            EiEvent::DeviceResumed(e) => {
                log::debug!("device resumed: {:?}", e.device().device_type());
                e.device().device().start_emulating(0, 0);
            }
            EiEvent::KeyboardModifiers(e) => {
                log::debug!("modifiers: {e:?}");
            }
            // only for receiver context
            // EiEvent::Frame(_) => { },
            // EiEvent::DeviceStartEmulating(_) => { },
            // EiEvent::DeviceStopEmulating(_) => { },
            // EiEvent::PointerMotion(_) => { },
            // EiEvent::PointerMotionAbsolute(_) => { },
            // EiEvent::Button(_) => { },
            // EiEvent::ScrollDelta(_) => { },
            // EiEvent::ScrollStop(_) => { },
            // EiEvent::ScrollCancel(_) => { },
            // EiEvent::ScrollDiscrete(_) => { },
            // EiEvent::KeyboardKey(_) => { },
            // EiEvent::TouchDown(_) => { },
            // EiEvent::TouchUp(_) => { },
            // EiEvent::TouchMotion(_) => { },
            _ => unreachable!("unexpected ei event"),
        }
        writer.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use reis::{PendingRequestResult, eis};
    use std::io::{ErrorKind, Read, Write};
    use std::{future::poll_fn, time::Duration};
    use tokio::io::AsyncReadExt;

    fn pressured_writer() -> (Arc<EiWriter>, UnixStream, usize) {
        let (client, server) = UnixStream::pair().expect("local EIS transport");
        client.set_nonblocking(true).unwrap();
        server.set_nonblocking(true).unwrap();
        let mut writable = client.try_clone().unwrap();
        let mut padding = 0;
        loop {
            match writable.write(&[0xaa; 8192]) {
                Ok(0) => panic!("socket stopped accepting data without EAGAIN"),
                Ok(count) => padding += count,
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => panic!("filling socket: {error}"),
            }
        }
        let context = ei::Context::new(client).unwrap();
        let writer = Arc::new(EiWriter::new(context, writable).unwrap());
        (writer, server, padding)
    }

    async fn receive_names(server: UnixStream, padding: usize, count: usize) -> Vec<String> {
        let mut stream = tokio::net::UnixStream::from_std(server).unwrap();
        stream.read_exact(&mut vec![0; padding]).await.unwrap();
        let server = eis::Context::new(stream.into_std().unwrap()).unwrap();
        let receiver = AsyncFd::with_interest(server, Interest::READABLE).unwrap();
        let mut names = Vec::new();
        while names.len() < count {
            let request = poll_fn(|cx| {
                loop {
                    if let Some(request) = receiver.get_ref().pending_request() {
                        return Poll::Ready(request);
                    }
                    let mut ready = std::task::ready!(receiver.poll_read_ready(cx)).unwrap();
                    receiver.get_ref().read().unwrap();
                    ready.clear_ready();
                }
            })
            .await;
            match request {
                PendingRequestResult::Request(eis::Request::Handshake(
                    _,
                    eis::handshake::Request::Name { name },
                )) => names.push(name),
                other => panic!("unexpected EIS request: {other:?}"),
            }
        }
        names
    }

    #[tokio::test]
    async fn backpressure_wakes_idle_dispatch_and_preserves_partial_writes_in_order() {
        let (writer, server, padding) = pressured_writer();
        let (registered_tx, registered_rx) = tokio::sync::oneshot::channel();
        let dispatch_writer = writer.clone();
        let dispatch = tokio::spawn(async move {
            let mut registered_tx = Some(registered_tx);
            poll_fn(|cx| {
                let result = dispatch_writer.poll_flush(cx);
                if let Some(tx) = registered_tx.take() {
                    tx.send(()).unwrap();
                }
                result
            })
            .await
        });
        registered_rx.await.unwrap();
        let expected = vec![
            "first".to_owned(),
            "x".repeat(512 * 1024),
            "last".to_owned(),
        ];
        for name in &expected {
            writer.context.handshake().name(name);
            writer
                .flush()
                .expect("backpressure must not disable emulation");
        }
        assert!(writer.pending.load(Ordering::Acquire));
        let (flushed, received) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(dispatch, receive_names(server, padding, expected.len()))
        })
        .await
        .expect("queued requests were not flushed without more input");
        flushed.unwrap().unwrap();
        assert_eq!(received, expected);
    }

    #[tokio::test]
    async fn cancelling_dispatch_keeps_pending_output_for_the_next_poll() {
        let (writer, server, padding) = pressured_writer();
        writer.context.handshake().name("once");
        writer.flush().unwrap();
        {
            let flush = poll_fn(|cx| writer.poll_flush(cx));
            tokio::pin!(flush);
            assert!(flush.as_mut().now_or_never().is_none());
        }
        let (flushed, received) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                poll_fn(|cx| writer.poll_flush(cx)),
                receive_names(server, padding, 1)
            )
        })
        .await
        .expect("cancelled dispatch lost its queued output");
        flushed.unwrap();
        assert_eq!(received, ["once"]);
    }

    #[tokio::test]
    async fn disconnect_during_backpressure_still_ends_the_session() {
        let (writer, server, _padding) = pressured_writer();
        writer.context.handshake().name("pending");
        writer.flush().unwrap();
        drop(server);
        let error =
            tokio::time::timeout(Duration::from_secs(5), poll_fn(|cx| writer.poll_flush(cx)))
                .await
                .expect("closed socket did not wake pending dispatch")
                .expect_err("closed socket must not be ignored as backpressure");
        assert!(matches!(error, EmulationError::EndOfStream));
    }

    #[test]
    fn reset_eis_socket_is_reported_as_session_end() {
        let (mut client, server) = UnixStream::pair().expect("local EIS transport");
        client.write_all(b"unread").expect("queue data for peer");
        drop(server);
        let error = client
            .read(&mut [0])
            .expect_err("closing a peer with unread data resets the socket");
        assert_eq!(error.kind(), ErrorKind::ConnectionReset);
        assert!(matches!(
            libei_stream_error(reis::Error::Io(error)),
            EmulationError::EndOfStream
        ));
    }

    #[test]
    fn closed_write_transports_are_reported_as_session_end() {
        for kind in [
            ErrorKind::ConnectionAborted,
            ErrorKind::BrokenPipe,
            ErrorKind::NotConnected,
            ErrorKind::UnexpectedEof,
        ] {
            assert!(
                matches!(libei_io_error(kind.into()), EmulationError::EndOfStream),
                "{kind:?} should trigger session recovery"
            );
        }
    }

    #[test]
    fn transport_permission_errors_are_not_treated_as_session_loss() {
        let error = libei_stream_error(reis::Error::Io(ErrorKind::PermissionDenied.into()));
        assert!(matches!(
            error,
            EmulationError::Io(error) if error.kind() == ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn protocol_errors_are_not_treated_as_session_loss() {
        assert!(matches!(
            libei_stream_error(reis::Error::UnexpectedHandshakeEvent),
            EmulationError::Libei(reis::Error::UnexpectedHandshakeEvent)
        ));
    }
}
