use std::collections::HashSet;

use tokio::{sync::mpsc::Sender, task::JoinHandle};

use crate::{client::ClientHandle, dns::DnsResolver};

use super::Server;

#[derive(Clone)]
pub struct DnsRequest {
    pub hostname: String,
    pub handle: ClientHandle,
}

pub fn new(resolver: DnsResolver, server: Server) -> (JoinHandle<()>, Sender<DnsRequest>) {
    let (dns_tx, mut dns_rx) = tokio::sync::mpsc::channel::<DnsRequest>(32);
    let resolver_task = tokio::task::spawn_local(async move {
        loop {
            let (host, handle) = match dns_rx.recv().await {
                Some(r) => (r.hostname, r.handle),
                None => break,
            };
            let ips = match resolver.resolve(&host).await {
                Ok(ips) => ips,
                Err(e) => {
                    log::warn!("could not resolve host '{host}': {e}");
                    continue;
                }
            };
            if let Some(state) = server.client_manager.borrow_mut().get_mut(handle) {
                let mut addrs = HashSet::from_iter(state.client.fix_ips.iter().cloned());
                for ip in ips {
                    addrs.insert(ip);
                }
                state.client.ips = addrs;
            }
        }
    });
    (resolver_task, dns_tx)
}
