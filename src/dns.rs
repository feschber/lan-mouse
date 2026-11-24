use crate::client::ClientManager;
use std::{collections::HashMap, future::Future, io, net::IpAddr};

use tokio::net::lookup_host;
use tokio::sync::{
    mpsc::{
        UnboundedReceiver as Receiver, UnboundedSender as Sender, unbounded_channel as channel,
    },
    oneshot, watch,
};
use tokio::task::{JoinHandle, spawn};

use tokio_util::sync::CancellationToken;

use lan_mouse_ipc::ClientHandle;

pub(crate) struct DnsResolver {
    cancellation_token: CancellationToken,
    task: Option<JoinHandle<()>>,
    request_tx: Sender<DnsRequest>,
    event_rx: Receiver<DnsEvent>,
}

#[derive(Clone)]
pub(crate) struct DnsLookup {
    request_tx: Sender<DnsRequest>,
}

impl DnsLookup {
    pub(crate) async fn resolve(
        &self,
        handle: ClientHandle,
        hostname: String,
        settings: watch::Receiver<bool>,
    ) -> bool {
        let (ready, response) = oneshot::channel();
        if self
            .request_tx
            .send(DnsRequest {
                handle,
                hostname,
                settings,
                ready: Some(ready),
            })
            .is_err()
        {
            return false;
        }
        response.await.is_ok()
    }
}

struct DnsRequest {
    handle: ClientHandle,
    hostname: String,
    settings: watch::Receiver<bool>,
    ready: Option<oneshot::Sender<()>>,
}

pub(crate) enum DnsEvent {
    Resolving(ClientHandle, watch::Receiver<bool>),
    Resolved {
        handle: ClientHandle,
        settings: watch::Receiver<bool>,
        ips: io::Result<Vec<IpAddr>>,
        ready: Option<oneshot::Sender<()>>,
    },
}

impl DnsEvent {
    pub(crate) fn apply(self, clients: &ClientManager) -> Option<ClientHandle> {
        match self {
            Self::Resolving(handle, settings) => {
                if !matches!(settings.has_changed(), Ok(false)) {
                    return None;
                }
                clients.set_resolving(handle, true);
                Some(handle)
            }
            Self::Resolved {
                handle,
                settings,
                ips,
                ready,
            } => {
                // A removed/reused handle or edited endpoint invalidates the result.
                if !matches!(settings.has_changed(), Ok(false)) {
                    return None;
                }
                clients.set_resolving(handle, false);
                if let Err(error) = &ips {
                    log::warn!("could not resolve client {handle}: {error}");
                }
                clients.set_dns_ips(handle, ips.unwrap_or_default());
                if let Some(ready) = ready {
                    let _ = ready.send(());
                }
                Some(handle)
            }
        }
    }
}

struct DnsTask {
    request_rx: Receiver<DnsRequest>,
    event_tx: Sender<DnsEvent>,
    cancellation_token: CancellationToken,
    active_tasks: HashMap<ClientHandle, JoinHandle<()>>,
}

impl DnsResolver {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self::with_lookup(resolve_hostname))
    }

    pub(crate) fn with_lookup<F, Fut>(lookup: F) -> Self
    where
        F: Fn(String) -> Fut + Clone + Send + 'static,
        Fut: Future<Output = io::Result<Vec<IpAddr>>> + Send + 'static,
    {
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let cancellation_token = CancellationToken::new();
        let dns_task = DnsTask {
            active_tasks: Default::default(),
            request_rx,
            event_tx,
            cancellation_token: cancellation_token.clone(),
        };
        let task = Some(spawn(dns_task.run(lookup)));
        Self {
            cancellation_token,
            task,
            event_rx,
            request_tx,
        }
    }

    pub(crate) fn lookup(&self) -> DnsLookup {
        DnsLookup {
            request_tx: self.request_tx.clone(),
        }
    }

    pub(crate) fn resolve(
        &self,
        handle: ClientHandle,
        hostname: String,
        settings: watch::Receiver<bool>,
    ) {
        let request = DnsRequest {
            handle,
            hostname,
            settings,
            ready: None,
        };
        let _ = self.request_tx.send(request);
    }

    pub(crate) async fn event(&mut self) -> DnsEvent {
        self.event_rx.recv().await.expect("channel closed")
    }

    pub(crate) async fn terminate(&mut self) {
        self.cancellation_token.cancel();
        self.task.take().expect("task").await.expect("join error");
    }
}

impl DnsTask {
    async fn run<F, Fut>(mut self, lookup: F)
    where
        F: Fn(String) -> Fut + Clone + Send + 'static,
        Fut: Future<Output = io::Result<Vec<IpAddr>>> + Send + 'static,
    {
        let cancellation_token = self.cancellation_token.clone();
        tokio::select! {
            _ = self.do_dns(lookup) => {},
            _ = cancellation_token.cancelled() => {},
        }
    }

    async fn do_dns<F, Fut>(&mut self, lookup: F)
    where
        F: Fn(String) -> Fut + Clone + Send + 'static,
        Fut: Future<Output = io::Result<Vec<IpAddr>>> + Send + 'static,
    {
        while let Some(dns_request) = self.request_rx.recv().await {
            let DnsRequest {
                handle,
                hostname,
                settings,
                ready,
            } = dns_request;

            /* abort previous dns task */
            let previous_task = self.active_tasks.remove(&handle);
            if let Some(task) = previous_task {
                if !task.is_finished() {
                    task.abort();
                }
            }

            self.event_tx
                .send(DnsEvent::Resolving(handle, settings.clone()))
                .expect("channel closed");

            /* spawn task for dns request */
            let event_tx = self.event_tx.clone();
            let cancellation_token = self.cancellation_token.clone();
            let lookup = lookup.clone();
            let mut settings = settings;

            let task = tokio::spawn(async move {
                tokio::select! {
                    biased;
                    _ = cancellation_token.cancelled() => {},
                    _ = settings.changed() => {},
                    result = lookup(hostname) => {
                       event_tx
                           .send(DnsEvent::Resolved { handle, settings, ips: result, ready })
                           .expect("channel closed");
                    }
                }
            });
            self.active_tasks.insert(handle, task);
        }
    }
}

/// Resolve `hostname` via the operating system's full name-resolution
/// stack (`getaddrinfo` on Unix, GetAddrInfoEx on Windows). This walks
/// `/etc/nsswitch.conf` on Linux — picking up mDNS via Avahi, /etc/hosts,
/// and DNS — and uses Bonjour for `.local` names on macOS. Pure-DNS
/// resolvers like hickory miss all of those, which is why a Bonjour
/// hostname (e.g. `JKMBP-M4-Max.local`) wouldn't resolve before.
///
/// Port `0` is a placeholder — `lookup_host` requires `host:port` but we
/// only care about the IPs at this stage; the actual port is appended at
/// connection time.
async fn resolve_hostname(hostname: String) -> io::Result<Vec<IpAddr>> {
    let addrs = lookup_host((hostname.as_str(), 0)).await?;
    Ok(addrs.map(|sa| sa.ip()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;

    #[tokio::test]
    async fn connection_lookup_waits_until_fresh_addresses_are_applied() {
        let clients = ClientManager::default();
        let handle = clients.add_client();
        clients.set_hostname(handle, Some("peer.local".into()));
        let ip = "127.0.0.2".parse().unwrap();
        let mut resolver = DnsResolver::with_lookup(move |_| std::future::ready(Ok(vec![ip])));
        let lookup = resolver.lookup();
        let request = lookup.resolve(
            handle,
            "peer.local".into(),
            clients.connection_changes(handle).unwrap(),
        );
        tokio::pin!(request);
        assert!(request.as_mut().now_or_never().is_none());
        resolver.event().await.apply(&clients);
        let resolved = resolver.event().await;
        assert!(request.as_mut().now_or_never().is_none());
        resolved.apply(&clients);
        assert!(request.await);
        assert_eq!(
            clients.get_ips(handle).unwrap(),
            std::collections::HashSet::from([ip])
        );
        resolver.terminate().await;
    }

    #[tokio::test]
    async fn endpoint_edits_reject_pending_dns_results() {
        let clients = ClientManager::default();
        let handle = clients.add_client();
        clients.set_hostname(handle, Some("old.local".into()));
        let settings = clients.connection_changes(handle).unwrap();
        let (ready, response) = oneshot::channel();
        clients.set_hostname(handle, Some("new.local".into()));
        let current = "127.0.0.2".parse().unwrap();
        clients.set_dns_ips(handle, vec![current]);
        DnsEvent::Resolved {
            handle,
            settings,
            ready: Some(ready),
            ips: Ok(vec!["127.0.0.1".parse().unwrap()]),
        }
        .apply(&clients);
        assert!(response.await.is_err());
        assert_eq!(
            clients.get_ips(handle).unwrap(),
            std::collections::HashSet::from([current])
        );
    }

    #[test]
    fn a_reused_handle_does_not_receive_the_previous_clients_dns_result() {
        let clients = ClientManager::default();
        let handle = clients.add_client();
        let settings = clients.connection_changes(handle).unwrap();
        clients.remove_client(handle);
        let replacement = clients.add_client();
        assert_eq!(handle, replacement);
        let current = "127.0.0.2".parse().unwrap();
        clients.set_dns_ips(replacement, vec![current]);
        DnsEvent::Resolved {
            handle,
            settings,
            ready: None,
            ips: Ok(vec!["127.0.0.1".parse().unwrap()]),
        }
        .apply(&clients);
        assert_eq!(
            clients.get_ips(replacement).unwrap(),
            std::collections::HashSet::from([current])
        );
    }
}
