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

impl DnsResolver {
    pub(crate) fn new() -> Result<Self, ResolveError> {
        let resolver = TokioAsyncResolver::tokio_from_system_conf()?;
        let (request_tx, request_rx) = channel();
        let (event_tx, event_rx) = channel();
        let cancellation_token = CancellationToken::new();
        let task = Some(spawn_local(Self::run(
            resolver,
            request_rx,
            event_tx,
            cancellation_token.clone(),
        )));
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

    async fn run(
        resolver: TokioAsyncResolver,
        mut request_rx: Receiver<DnsRequest>,
        event_tx: Sender<DnsEvent>,
        cancellation_token: CancellationToken,
    ) {
        tokio::select! {
            _ = Self::do_dns(&resolver, &mut request_rx, &event_tx) => {},
            _ = cancellation_token.cancelled() => {},
        }
    }

    async fn do_dns(
        resolver: &TokioAsyncResolver,
        request_rx: &mut Receiver<DnsRequest>,
        event_tx: &Sender<DnsEvent>,
    ) {
        loop {
            let DnsRequest { handle, hostname } = request_rx.recv().await.expect("channel closed");

            event_tx
                .send(DnsEvent::Resolving(handle))
                .expect("channel closed");

            /* resolve host */
            let ips = resolver
                .lookup_ip(&hostname)
                .await
                .map(|ips| ips.iter().collect::<Vec<_>>());

            event_tx
                .send(DnsEvent::Resolved(handle, hostname, ips))
                .expect("channel closed");
        }
    }

    pub(crate) async fn terminate(&mut self) {
        self.cancellation_token.cancel();
        self.task.take().expect("task").await.expect("join error");
    }
}
