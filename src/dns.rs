use anyhow::Result;
use std::net::IpAddr;
use tokio::sync::mpsc::Receiver;

use hickory_resolver::{error::ResolveError, TokioAsyncResolver};

use crate::{client::ClientHandle, server::Server};

pub(crate) struct DnsResolver {
    resolver: TokioAsyncResolver,
    dns_request: Receiver<ClientHandle>,
}

impl DnsResolver {
    pub(crate) fn new(dns_request: Receiver<ClientHandle>) -> Result<Self> {
        let resolver = TokioAsyncResolver::tokio_from_system_conf()?;
        Ok(Self {
            resolver,
            dns_request,
        })
    }

    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, ResolveError> {
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
            let hostname = match server.get_hostname(handle) {
                Some(hostname) => hostname,
                None => continue,
            };

            log::info!("resolving ({handle}) `{hostname}` ...");
            server.set_resolving(handle, true);

            let ips = match self.resolve(&hostname).await {
                Ok(ips) => ips,
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
