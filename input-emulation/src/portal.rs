use std::{
    env, fs, io,
    path::{Path, PathBuf},
    task::{Context, Poll},
};

use ashpd::{
    desktop::{
        PersistMode, Session,
        inhibit::InhibitProxy,
        remote_desktop::{DeviceType, RemoteDesktop, SelectDevicesOptions},
    },
    zbus::{self, zvariant},
};
use futures::{StreamExt, stream::BoxStream};

pub(super) struct RemoteDesktopSession {
    pub(super) proxy: RemoteDesktop,
    pub(super) session: Session<RemoteDesktop>,
    pub(super) closed: SessionClosed,
}

pub(super) struct SessionClosed {
    signals: BoxStream<'static, ()>,
    closed: bool,
}

impl SessionClosed {
    async fn new(
        connection: zbus::Connection,
        session: &Session<RemoteDesktop>,
    ) -> Result<Self, ashpd::Error> {
        // ashpd does not expose the session path, and receive_closed() captures
        // &Session in its opaque return type. Its object-path serialization lets
        // us subscribe to the same signal without a self-referential stream.
        let encoded = zvariant::to_bytes(
            zvariant::serialized::Context::new_dbus(zvariant::LE, 0),
            session,
        )?;
        let (path, _) = encoded.deserialize::<zvariant::OwnedObjectPath>()?;
        let proxy = zbus::Proxy::new_owned(
            connection,
            "org.freedesktop.portal.Desktop",
            path,
            "org.freedesktop.portal.Session",
        )
        .await?;
        let signals = proxy.receive_signal("Closed").await?.map(|_| ()).boxed();
        Ok(Self {
            signals,
            closed: false,
        })
    }

    pub(super) fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.closed || self.signals.as_mut().poll_next(cx).is_ready() {
            self.closed = true;
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl RemoteDesktopSession {
    pub(super) async fn new() -> Result<Self, ashpd::Error> {
        // Cancellation drops this private connection and its portal resources,
        // including a lock-state monitor or a pending authorization request.
        let connection = zbus::Connection::session().await?;
        if let Err(error) = wait_for_session_unlock(connection.clone()).await {
            // Session monitoring is not implemented by every desktop portal.
            log::warn!("could not monitor desktop lock state before input emulation: {error}");
        }
        let proxy = RemoteDesktop::with_connection(connection.clone()).await?;
        let token_path = get_token_file_path();
        let mut restore_token = read_token(&token_path);

        let (session, closed) = loop {
            log::debug!("creating RemoteDesktop session ...");
            let session = proxy.create_session(Default::default()).await?;
            let closed = SessionClosed::new(connection.clone(), &session).await?;
            let options = SelectDevicesOptions::default()
                .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
                .set_persist_mode(PersistMode::ExplicitlyRevoked)
                .set_restore_token(restore_token.as_deref());
            let selection = proxy
                .select_devices(&session, options)
                .await
                .and_then(|request| request.response());
            match selection {
                Ok(()) => break (session, closed),
                Err(error) => {
                    let rejected = take_rejected_token(&mut restore_token, &error);
                    if let Err(close_error) = session.close().await {
                        log::debug!(
                            "failed to close rejected RemoteDesktop session: {close_error}"
                        );
                    }
                    let Some(rejected) = rejected else {
                        return Err(error);
                    };
                    log::warn!(
                        "cached RemoteDesktop token was rejected; requesting fresh permission"
                    );
                    if let Err(error) = invalidate_token(&token_path, &rejected) {
                        log::warn!("failed to remove rejected RemoteDesktop token: {error}");
                    }
                }
            }
        };

        log::info!("requesting permission for input emulation");
        let response = proxy
            .start(&session, None, Default::default())
            .await?
            .response()?;
        // Restore tokens are single-use; persist the replacement, not the old token.
        if let Some(token) = response.restore_token() {
            if let Err(error) = write_token(&token_path, token) {
                log::warn!("failed to save RemoteDesktop token: {error}");
            }
        }
        Ok(Self {
            proxy,
            session,
            closed,
        })
    }

    pub(super) fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        self.closed.poll_closed(cx)
    }

    pub(super) async fn close(&mut self) {
        if !self.closed.closed {
            self.closed.closed = true;
            if let Err(error) = self.session.close().await {
                log::debug!("failed to close RemoteDesktop session: {error}");
            }
        }
    }
}

fn take_rejected_token(token: &mut Option<String>, error: &ashpd::Error) -> Option<String> {
    if matches!(
        error,
        ashpd::Error::Portal(ashpd::PortalError::InvalidArgument(_))
    ) {
        token.take()
    } else {
        None
    }
}

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

fn read_token(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|token| token.trim().to_string())
}

fn write_token(path: &Path, token: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, token)
}

fn invalidate_token(path: &Path, rejected: &str) -> io::Result<()> {
    // Another backend may already have saved a replacement token.
    if read_token(path).as_deref() == Some(rejected) {
        fs::remove_file(path)?;
    }
    Ok(())
}

async fn wait_until_unlocked(
    mut states: impl futures::Stream<Item = bool> + Unpin,
) -> Result<(), ashpd::Error> {
    while let Some(screensaver_active) = states.next().await {
        if !screensaver_active {
            return Ok(());
        }
        log::info!("waiting for desktop unlock before creating input emulation");
    }
    Err(ashpd::Error::NoResponse)
}

async fn wait_for_session_unlock(connection: zbus::Connection) -> Result<(), ashpd::Error> {
    let proxy = InhibitProxy::with_connection(connection).await?;
    // Subscribe before creating the monitor so its initial state is not lost.
    let states = proxy.receive_state_changed().await?;
    let monitor = proxy.create_monitor(None, Default::default()).await?;
    let result = async {
        let mut closed = monitor.receive_closed().await?;
        tokio::select! {
            biased;
            _ = closed.next() => Err(ashpd::Error::NoResponse),
            result = wait_until_unlocked(states.map(|state| state.screensaver_active())) => result,
        }
    }
    .await;
    if let Err(error) = monitor.close().await {
        log::debug!("failed to close session state monitor: {error}");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{FutureExt, task::noop_waker_ref};

    #[test]
    fn cached_token_rejection_allows_only_one_fresh_attempt() {
        let mut token = Some("opaque backend token, not a UUID".to_string());
        let error =
            ashpd::Error::Portal(ashpd::PortalError::InvalidArgument("restore token".into()));
        assert!(take_rejected_token(&mut token, &error).is_some());
        assert!(take_rejected_token(&mut token, &error).is_none());
    }

    #[test]
    fn permission_denied_and_cancellation_do_not_consume_cached_token() {
        for error in [
            ashpd::Error::Portal(ashpd::PortalError::NotAllowed("denied".into())),
            ashpd::Error::Portal(ashpd::PortalError::Cancelled("cancelled".into())),
            ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled),
            ashpd::Error::Response(ashpd::desktop::ResponseError::Other),
            ashpd::Error::Portal(ashpd::PortalError::Failed("InvalidArgument".into())),
        ] {
            let mut token = Some("opaque token".to_string());
            assert!(take_rejected_token(&mut token, &error).is_none());
            assert_eq!(token.as_deref(), Some("opaque token"));
        }
    }

    #[test]
    fn invalid_argument_without_cached_token_does_not_retry() {
        let error = ashpd::Error::Portal(ashpd::PortalError::InvalidArgument("devices".into()));
        assert!(take_rejected_token(&mut None, &error).is_none());
    }

    #[test]
    fn invalidating_rejected_token_preserves_a_replacement() {
        let directory = std::env::temp_dir().join(format!(
            "lan-mouse-token-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let path = directory.join("remote-desktop.token");
        write_token(&path, "replacement opaque token").expect("write replacement");
        invalidate_token(&path, "rejected opaque token").expect("invalidate old token");
        assert_eq!(
            read_token(&path).as_deref(),
            Some("replacement opaque token")
        );
        invalidate_token(&path, "replacement opaque token").expect("invalidate matching token");
        assert!(!path.exists());
        fs::remove_dir(directory).expect("remove isolated cache directory");
    }

    #[test]
    fn session_closed_signal_and_eof_are_latched() {
        for signals in [
            futures::stream::once(futures::future::ready(()))
                .chain(futures::stream::pending())
                .boxed(),
            futures::stream::empty().boxed(),
        ] {
            let mut closed = SessionClosed {
                signals,
                closed: false,
            };
            let mut cx = Context::from_waker(noop_waker_ref());
            assert!(closed.poll_closed(&mut cx).is_ready());
            assert!(closed.poll_closed(&mut cx).is_ready());
        }
    }

    #[test]
    fn live_session_does_not_report_closed() {
        let mut closed = SessionClosed {
            signals: futures::stream::pending().boxed(),
            closed: false,
        };
        assert!(
            closed
                .poll_closed(&mut Context::from_waker(noop_waker_ref()))
                .is_pending()
        );
    }

    #[tokio::test]
    async fn unlocked_desktop_allows_immediate_session_creation() {
        let waiting = wait_until_unlocked(futures::stream::iter([false]));
        assert!(matches!(waiting.now_or_never(), Some(Ok(()))));
    }

    #[tokio::test]
    async fn locked_desktop_defers_creation_until_unlock() {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let waiting = wait_until_unlocked(rx);
        tokio::pin!(waiting);
        tx.unbounded_send(true).expect("state receiver");
        assert!(waiting.as_mut().now_or_never().is_none());
        tx.unbounded_send(false).expect("state receiver");
        assert!(matches!(waiting.as_mut().now_or_never(), Some(Ok(()))));
    }

    #[tokio::test]
    async fn closed_monitor_does_not_report_a_locked_desktop_as_unlocked() {
        assert!(matches!(
            wait_until_unlocked(futures::stream::iter([true])).await,
            Err(ashpd::Error::NoResponse)
        ));
    }
}
