use anyhow::Result;
use std::{collections::HashSet, error::Error, net::IpAddr};
use tokio::sync::mpsc::{channel, Receiver, Sender};

use hickory_resolver::TokioAsyncResolver;

use crate::{client::ClientHandle, server::Server};

pub(crate) struct DnsResolver {
    resolver: TokioAsyncResolver,
    dns_request: Receiver<ClientHandle>,
}

impl DnsResolver {
    pub(crate) async fn new() -> Result<(Self, Sender<ClientHandle>)> {
        let resolver = TokioAsyncResolver::tokio_from_system_conf()?;
        let (dns_tx, dns_request) = channel(1);
        Ok((
            Self {
                resolver,
                dns_request,
            },
            dns_tx,
        ))
    }

    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, Box<dyn Error>> {
        let response = self.resolver.lookup_ip(host).await?;
        for ip in response.iter() {
            log::info!("{host}: adding ip {ip}");
        }
        Ok(response.iter().collect())
    }

    pub(crate) async fn run(mut self, server: Server) {
        tokio::select! {
            _ = server.cancelled() => {},
            _ = self.do_dns(&server) => {},
        }
    }

    async fn do_dns(&mut self, server: &Server) {
        loop {
            let handle = self.dns_request.recv().await.expect("channel closed");

            /* update resolving status */
            let hostname = if let Some((c, s)) = server.client_manager.borrow_mut().get_mut(handle)
            {
                s.resolving = true;
                c.hostname.clone()
            } else {
                continue;
            };
            let Some(hostname) = hostname else {
                continue;
            };

            /* FIXME race -> need some other event */
            server.client_resolved(handle);

            log::info!("resolving ({handle}) `{hostname}` ...");
            let ips = match self.resolve(&hostname).await {
                Ok(ips) => ips,
                Err(e) => {
                    log::warn!("could not resolve host '{hostname}': {e}");
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
            server.client_resolved(handle);
        }
    }
}
