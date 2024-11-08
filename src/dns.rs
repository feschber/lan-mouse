use std::net::IpAddr;

use local_channel::mpsc::{channel, Receiver, Sender};
use tokio::task::{spawn_local, JoinHandle};

use hickory_resolver::{error::ResolveError, TokioAsyncResolver};
use tokio_util::sync::CancellationToken;

use lan_mouse_ipc::ClientHandle;

pub(crate) struct DnsResolver {
    cancellation_token: CancellationToken,
    task: Option<JoinHandle<()>>,
    request_tx: Sender<DnsRequest>,
    event_rx: Receiver<DnsEvent>,
}

struct DnsRequest {
    handle: ClientHandle,
    hostname: String,
}

pub(crate) enum DnsEvent {
    Resolving(ClientHandle),
    Resolved(ClientHandle, String, Result<Vec<IpAddr>, ResolveError>),
}

struct DnsTask {
    resolver: TokioAsyncResolver,
    request_rx: Receiver<DnsRequest>,
    event_tx: Sender<DnsEvent>,
    cancellation_token: CancellationToken,
}

impl DnsResolver {
    pub(crate) fn new() -> Result<Self, ResolveError> {
        let resolver = TokioAsyncResolver::tokio_from_system_conf()?;
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let cancellation_token = CancellationToken::new();
        let dns_task = DnsTask {
            resolver,
            request_rx,
            event_tx,
            cancellation_token: cancellation_token.clone(),
        };
        let task = Some(spawn_local(dns_task.run()));
        Ok(Self {
            cancellation_token,
            task,
            event_rx,
            request_tx,
        })
    }

    pub(crate) fn resolve(&self, handle: ClientHandle, hostname: String) {
        let request = DnsRequest { handle, hostname };
        self.request_tx.send(request).expect("channel closed");
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
    async fn run(mut self) {
        let cancellation_token = self.cancellation_token.clone();
        tokio::select! {
            _ = self.do_dns() => {},
            _ = cancellation_token.cancelled() => {},
        }
    }

    async fn do_dns(&mut self) {
        while let Some(dns_request) = self.request_rx.recv().await {
            let DnsRequest { handle, hostname } = dns_request;

            self.event_tx
                .send(DnsEvent::Resolving(handle))
                .expect("channel closed");

            /* spawn task for dns request */
            let event_tx = self.event_tx.clone();
            let resolver = self.resolver.clone();
            let cancellation_token = self.cancellation_token.clone();

            tokio::task::spawn_local(async move {
                tokio::select! {
                    ips = resolver.lookup_ip(&hostname) => {
                       let ips = ips.map(|ips| ips.iter().collect::<Vec<_>>());
                       event_tx
                           .send(DnsEvent::Resolved(handle, hostname, ips))
                           .expect("channel closed");
                    }
                    _ = cancellation_token.cancelled() => {},
                }
            });
        }
    }
}
