use local_channel::mpsc::{channel, Receiver, Sender};
use tokio::task::{spawn_local, JoinHandle};

use hickory_resolver::{error::ResolveError, TokioAsyncResolver};

use crate::server::Server;
use lan_mouse_ipc::ClientHandle;

pub(crate) struct DnsResolver {
    _task: JoinHandle<()>,
    tx: Sender<ClientHandle>,
}

impl DnsResolver {
    pub(crate) fn new(server: Server) -> Result<Self, ResolveError> {
        let resolver = TokioAsyncResolver::tokio_from_system_conf()?;
        let (tx, rx) = channel();
        let _task = spawn_local(Self::run(server, resolver, rx));
        Ok(Self { _task, tx })
    }

    pub(crate) fn resolve(&self, host: ClientHandle) {
        self.tx.send(host).expect("channel closed");
    }

    async fn run(server: Server, resolver: TokioAsyncResolver, mut rx: Receiver<ClientHandle>) {
        tokio::select! {
            _ = server.cancelled() => {},
            _ = Self::do_dns(&server, &resolver, &mut rx) => {},
        }
    }

    async fn do_dns(
        server: &Server,
        resolver: &TokioAsyncResolver,
        rx: &mut Receiver<ClientHandle>,
    ) {
        loop {
            let handle = rx.recv().await.expect("channel closed");

            /* update resolving status */
            let hostname = match server.get_hostname(handle) {
                Some(hostname) => hostname,
                None => continue,
            };

            log::info!("resolving ({handle}) `{hostname}` ...");
            server.set_resolving(handle, true);

            /* resolve host */
            let ips = match resolver.lookup_ip(&hostname).await {
                Ok(response) => {
                    let ips = response.iter().collect::<Vec<_>>();
                    for ip in ips.iter() {
                        log::info!("{hostname}: adding ip {ip}");
                    }
                    ips
                }
                Err(e) => {
                    log::warn!("could not resolve host '{hostname}': {e}");
                    vec![]
                }
            };

            server.update_dns_ips(handle, ips);
            server.set_resolving(handle, false);
        }
    }
}
