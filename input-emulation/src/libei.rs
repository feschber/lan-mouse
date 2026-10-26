use futures::{StreamExt, future};
use std::{
    env, fs, io,
    os::{
        fd::{AsFd, OwnedFd},
        unix::net::UnixStream,
    },
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{Interest, unix::AsyncFd},
    sync::Notify,
    task::JoinHandle,
    time::Instant,
};

use ashpd::desktop::{
    PersistMode, Session,
    remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions},
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

use crate::error::EmulationError;

use super::{Emulation, EmulationHandle, error::LibeiEmulationCreationError};

#[derive(Clone, Default)]
struct Devices {
    pointer: Arc<RwLock<Option<(ei::Device, ei::Pointer)>>>,
    scroll: Arc<RwLock<Option<(ei::Device, ei::Scroll)>>>,
    button: Arc<RwLock<Option<(ei::Device, ei::Button)>>>,
    keyboard: Arc<RwLock<Option<(ei::Device, ei::Keyboard)>>>,
}

/// How long buffered requests may stay unsent while the socket is full before the
/// connection is considered stalled.
const FLUSH_STALL_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct LibeiEmulation {
    context: ei::Context,
    /// Wakes the flush task when a flush hit `WouldBlock`.
    retry_flush: Arc<Notify>,
    conn: event::Connection,
    devices: Devices,
    ei_task: JoinHandle<()>,
    flush_task: JoinHandle<()>,
    error: Arc<Mutex<Option<EmulationError>>>,
    libei_error: Arc<AtomicBool>,
    _remote_desktop: RemoteDesktop,
    session: Session<RemoteDesktop>,
}

/// Get the path to the RemoteDesktop token file
fn get_token_file_path() -> PathBuf {
    let cache_dir = env::var("XDG_CACHE_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = env::var("HOME").expect("HOME not set");
            PathBuf::from(home).join(".cache")
        });

    cache_dir.join("lan-mouse").join("remote-desktop.token")
}

/// Read the RemoteDesktop token from file
fn read_token() -> Option<String> {
    let token_path = get_token_file_path();
    match fs::read_to_string(&token_path) {
        Ok(token) => Some(token.trim().to_string()),
        Err(_) => None,
    }
}

/// Write the RemoteDesktop token to file
fn write_token(token: &str) -> io::Result<()> {
    let token_path = get_token_file_path();
    if let Some(parent) = token_path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(&token_path, token)?;
    Ok(())
}

async fn get_ei_fd() -> Result<(RemoteDesktop, Session<RemoteDesktop>, OwnedFd), ashpd::Error> {
    let remote_desktop = RemoteDesktop::new().await?;

    let restore_token = read_token();

    log::debug!("creating session ...");
    let session = remote_desktop.create_session(Default::default()).await?;

    log::debug!("selecting devices ...");
    let options = SelectDevicesOptions::default()
        .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
        .set_persist_mode(PersistMode::ExplicitlyRevoked)
        .set_restore_token(restore_token.as_deref());
    remote_desktop.select_devices(&session, options).await?;

    log::info!("requesting permission for input emulation");
    let start_response = remote_desktop
        .start(&session, None, Default::default())
        .await?
        .response()?;

    // The restore token is only valid once, we need to re-save it each time
    if let Some(token_str) = start_response.restore_token() {
        if let Err(e) = write_token(token_str) {
            log::warn!("failed to save RemoteDesktop token: {}", e);
        }
    }

    let fd = remote_desktop
        .connect_to_eis(&session, Default::default())
        .await?;
    Ok((remote_desktop, session, fd))
}

impl LibeiEmulation {
    pub(crate) async fn new() -> Result<Self, LibeiEmulationCreationError> {
        let (_remote_desktop, session, eifd) = get_ei_fd().await?;
        let stream = UnixStream::from(eifd);
        stream.set_nonblocking(true)?;
        let context = ei::Context::new(stream)?;
        let (conn, events) = context
            .handshake_tokio("de.feschber.LanMouse", ContextType::Sender)
            .await?;
        let devices = Devices::default();
        let libei_error = Arc::new(AtomicBool::default());
        let error = Arc::new(Mutex::new(None));
        let retry_flush = Arc::new(Notify::new());
        // The reis event stream already registers the socket for readability, so
        // writable readiness is tracked on a duplicate of the descriptor.
        let write_ready =
            AsyncFd::with_interest(context.as_fd().try_clone_to_owned()?, Interest::WRITABLE)?;
        let flush_handler = {
            let context = context.clone();
            let retry_flush = retry_flush.clone();
            let libei_error = libei_error.clone();
            let error = error.clone();
            async move {
                let e = flush_task(&context, &write_ready, &retry_flush, FLUSH_STALL_TIMEOUT).await;
                log::warn!("libei flush failed: {e}");
                error.lock().unwrap().get_or_insert(e);
                libei_error.store(true, Ordering::SeqCst);
            }
        };
        let flush_task = tokio::task::spawn_local(flush_handler);
        let ei_handler = ei_task(
            events,
            conn.clone(),
            context.clone(),
            retry_flush.clone(),
            devices.clone(),
            libei_error.clone(),
            error.clone(),
        );
        let ei_task = tokio::task::spawn_local(ei_handler);

        Ok(Self {
            context,
            retry_flush,
            conn,
            devices,
            ei_task,
            flush_task,
            error,
            libei_error,
            _remote_desktop,
            session,
        })
    }
}

impl Drop for LibeiEmulation {
    fn drop(&mut self) {
        self.ei_task.abort();
        self.flush_task.abort();
    }
}

#[async_trait]
impl Emulation for LibeiEmulation {
    async fn consume(
        &mut self,
        event: Event,
        _handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
        check_connection(&self.libei_error, &self.error)?;
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
        flush_or_retry_later(&self.context, &self.retry_flush)?;
        Ok(())
    }

    async fn create(&mut self, _: EmulationHandle) {}
    async fn destroy(&mut self, _: EmulationHandle) {}

    async fn terminate(&mut self) {
        let _ = self.session.close().await;
        self.ei_task.abort();
        self.flush_task.abort();
    }
}

/// Fails if the connection is unusable.
///
/// The error that ended the connection is returned once. Every call after that fails
/// as well: callers like `InputEmulation::destroy` discard the error, and a session
/// that stays alive would keep queueing requests that can never be sent.
fn check_connection(
    libei_error: &AtomicBool,
    error: &Mutex<Option<EmulationError>>,
) -> Result<(), EmulationError> {
    if !libei_error.load(Ordering::SeqCst) {
        return Ok(());
    }
    Err(error
        .lock()
        .unwrap()
        .take()
        .unwrap_or(EmulationError::EndOfStream))
}

/// Sends buffered requests without waiting for the socket.
///
/// If the socket is full, `reis` keeps the unsent bytes in its write buffer
/// (`wire::backend::Buffer::flush_write`), so nothing is lost. The flush task is woken
/// to send them once the socket accepts data again.
fn flush_or_retry_later(context: &ei::Context, retry_flush: &Notify) -> io::Result<()> {
    match context.flush().map_err(io::Error::from) {
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            retry_flush.notify_one();
            Ok(())
        }
        result => result,
    }
}

/// Sends buffered requests that did not fit into the socket.
///
/// Waits to be woken by [`flush_or_retry_later`], then retries until everything is sent.
/// Only returns on failure, either because the socket reported an error or because it
/// stayed full for `stall_timeout`. A compositor that stopped reading would otherwise
/// grow the write buffer forever.
async fn flush_task(
    context: &ei::Context,
    write_ready: &AsyncFd<OwnedFd>,
    retry_flush: &Notify,
    stall_timeout: Duration,
) -> EmulationError {
    loop {
        retry_flush.notified().await;
        if let Err(e) = flush_until_sent(context, write_ready, stall_timeout).await {
            return e;
        }
    }
}

async fn flush_until_sent(
    context: &ei::Context,
    write_ready: &AsyncFd<OwnedFd>,
    stall_timeout: Duration,
) -> Result<(), EmulationError> {
    let deadline = Instant::now() + stall_timeout;
    loop {
        match context.flush().map_err(io::Error::from) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                log::debug!("libei socket is full, waiting for it to become writable");
            }
            Err(e) => return Err(e.into()),
        }
        // Readiness is edge triggered: clear it before flushing again, so a socket that
        // is still full is waited for instead of reported as ready.
        tokio::time::timeout_at(deadline, write_ready.writable())
            .await
            .map_err(|_| EmulationError::LibeiStalled(stall_timeout))??
            .clear_ready();
    }
}

async fn ei_task(
    mut events: EiConvertEventStream,
    _conn: Connection,
    context: ei::Context,
    retry_flush: Arc<Notify>,
    devices: Devices,
    libei_error: Arc<AtomicBool>,
    error: Arc<Mutex<Option<EmulationError>>>,
) {
    loop {
        match ei_event_handler(&mut events, &context, &retry_flush, &devices).await {
            Ok(()) => {}
            Err(e) => {
                libei_error.store(true, Ordering::SeqCst);
                error.lock().unwrap().replace(e);
                // wait for termination -> otherwise we will loop forever
                future::pending::<()>().await;
            }
        }
    }
}

async fn ei_event_handler(
    events: &mut EiConvertEventStream,
    context: &ei::Context,
    retry_flush: &Notify,
    devices: &Devices,
) -> Result<(), EmulationError> {
    loop {
        let event = events.next().await.ok_or(EmulationError::EndOfStream)??;
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
        flush_or_retry_later(context, retry_flush)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tokio::io::AsyncReadExt;

    /// Bytes of payload per buffered message, well below the libei message size limit.
    const MESSAGE_PAYLOAD: usize = 1000;

    struct FullSocket {
        /// Keeps the read registration alive, like the reis event stream does.
        _read_ready: AsyncFd<UnixStream>,
        context: ei::Context,
        write_ready: AsyncFd<OwnedFd>,
        peer: UnixStream,
        /// Payload bytes still waiting to be sent to `peer`.
        payload: usize,
    }

    /// A libei context with a full send buffer and unsent messages in its write buffer.
    ///
    /// Messages are added until the kernel reports `WouldBlock`, so the setup does not
    /// depend on the size of the socket buffer.
    fn full_socket() -> FullSocket {
        let (socket, peer) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let read_ready = AsyncFd::with_interest(socket, Interest::READABLE).unwrap();
        let context = ei::Context::new(read_ready.get_ref().try_clone().unwrap()).unwrap();
        let write_ready = AsyncFd::with_interest(
            context.as_fd().try_clone_to_owned().unwrap(),
            Interest::WRITABLE,
        )
        .unwrap();

        let message = "x".repeat(MESSAGE_PAYLOAD);
        let mut payload = 0;
        loop {
            context.handshake().interface_version(&message, 1);
            payload += MESSAGE_PAYLOAD;
            match context.flush().map_err(io::Error::from) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("unexpected error while filling the socket: {e}"),
            }
            assert!(payload < 256 << 20, "socket buffer never filled up");
        }
        FullSocket {
            _read_ready: read_ready,
            context,
            write_ready,
            peer,
            payload,
        }
    }

    fn count_payload(bytes: &[u8]) -> usize {
        bytes.iter().filter(|&&b| b == b'x').count()
    }

    /// What `consume` does when the socket is full, so the flush task is woken the same
    /// way as in production.
    fn buffer_while_full(socket: &FullSocket, retry_flush: &Notify) {
        flush_or_retry_later(&socket.context, retry_flush).expect("a full socket is not an error");
    }

    #[tokio::test]
    async fn flush_or_retry_later_wakes_the_flush_task() {
        let socket = full_socket();
        let retry_flush = Notify::new();

        buffer_while_full(&socket, &retry_flush);

        tokio::time::timeout(Duration::from_millis(100), retry_flush.notified())
            .await
            .expect("flush task was not woken");
    }

    #[tokio::test]
    async fn flush_task_sends_everything_once_peer_reads() {
        let socket = full_socket();
        let retry_flush = Notify::new();
        buffer_while_full(&socket, &retry_flush);
        let flush = flush_task(
            &socket.context,
            &socket.write_ready,
            &retry_flush,
            Duration::from_secs(10),
        );
        tokio::pin!(flush);

        // The peer is not reading, so nothing completes and nothing fails.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut flush)
                .await
                .is_err()
        );

        socket.peer.set_nonblocking(true).unwrap();
        let mut peer = tokio::net::UnixStream::from_std(socket.peer).unwrap();
        let expected = socket.payload;
        let drain = async {
            let mut seen = 0;
            let mut buf = [0u8; 64 * 1024];
            while seen < expected {
                let n = peer.read(&mut buf).await.unwrap();
                assert!(n > 0, "peer reached EOF before all data arrived");
                seen += count_payload(&buf[..n]);
            }
            seen
        };
        tokio::select! {
            e = &mut flush => panic!("flush task failed: {e}"),
            seen = tokio::time::timeout(Duration::from_secs(10), drain) => {
                assert_eq!(seen.expect("data did not arrive in time"), expected);
            }
        }
    }

    #[tokio::test]
    async fn flush_task_reports_a_stalled_socket() {
        let socket = full_socket();
        let retry_flush = Notify::new();
        buffer_while_full(&socket, &retry_flush);

        let result = tokio::time::timeout(
            Duration::from_secs(5),
            flush_task(
                &socket.context,
                &socket.write_ready,
                &retry_flush,
                Duration::from_millis(100),
            ),
        )
        .await
        .expect("flush task did not give up");

        assert!(
            matches!(result, EmulationError::LibeiStalled(_)),
            "{result}"
        );
        // the peer is still connected and gets the prefix the kernel accepted
        let mut peer = socket.peer;
        peer.set_nonblocking(true).unwrap();
        let mut buf = [0u8; 4096];
        assert!(peer.read(&mut buf).unwrap() > 0);
    }

    #[test]
    fn connection_failure_is_reported_on_every_call() {
        let libei_error = AtomicBool::new(false);
        let error = Mutex::new(None);
        assert!(check_connection(&libei_error, &error).is_ok());

        error
            .lock()
            .unwrap()
            .replace(EmulationError::LibeiStalled(Duration::from_secs(5)));
        libei_error.store(true, Ordering::SeqCst);

        // the original error is returned once, later calls must not succeed again
        assert!(matches!(
            check_connection(&libei_error, &error),
            Err(EmulationError::LibeiStalled(_))
        ));
        assert!(check_connection(&libei_error, &error).is_err());
        assert!(check_connection(&libei_error, &error).is_err());
    }
}
