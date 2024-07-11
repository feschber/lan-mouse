use std::collections::HashSet;

use tokio::{
    sync::mpsc::{Receiver, Sender},
    task::JoinHandle,
};

use crate::{client::ClientHandle, dns::DnsResolver, frontend::FrontendEvent};

use super::Server;

#[derive(Clone)]
pub struct DnsRequest {
    pub hostname: String,
    pub handle: ClientHandle,
}

pub fn new(
    server: Server,
    resolver: DnsResolver,
    dns_rx: Receiver<DnsRequest>,
    frontend: Sender<FrontendEvent>,
) -> JoinHandle<()> {
    tokio::task::spawn_local(async move {
        tokio::select! {
            _ = server.cancelled() => {},
            _ = do_dns(&server, resolver, frontend, dns_rx) => {},
        }
    })
}

async fn do_dns(
    server: &Server,
    resolver: DnsResolver,
    frontend: Sender<FrontendEvent>,
    mut dns_rx: Receiver<DnsRequest>,
) {
    loop {
        let (host, handle) = match dns_rx.recv().await {
            Some(r) => (r.hostname, r.handle),
            None => break,
        };

        /* update resolving status */
        if let Some((_, s)) = server.client_manager.borrow_mut().get_mut(handle) {
            s.resolving = true;
        }
        notify_state_change(&frontend, server, handle).await;

        let ips = match resolver.resolve(&host).await {
            Ok(ips) => ips,
            Err(e) => {
                log::warn!("could not resolve host '{host}': {e}");
                vec![]
            }
        };

        /* update ips and resolving state */
        if let Some((c, s)) = server.client_manager.borrow_mut().get_mut(handle) {
            let mut addrs = HashSet::from_iter(c.fix_ips.iter().cloned());
            for ip in ips {
                addrs.insert(ip);
            }
            s.ips = addrs;
            s.resolving = false;
        }
        notify_state_change(&frontend, server, handle).await;
    }
}

async fn notify_state_change(
    frontend: &Sender<FrontendEvent>,
    server: &Server,
    handle: ClientHandle,
) {
    let state = server.client_manager.borrow().get(handle).cloned();
    if let Some((config, state)) = state {
        let _ = frontend
            .send(FrontendEvent::State(handle, config, state))
            .await;
    }
}
