use anyhow::Result;
use std::{error::Error, net::IpAddr};

use trust_dns_resolver::TokioAsyncResolver;

pub struct DnsResolver {
    resolver: TokioAsyncResolver,
}
impl DnsResolver {
    pub(crate) async fn new() -> Result<Self> {
        let resolver = TokioAsyncResolver::tokio_from_system_conf()?;
        Ok(Self { resolver })
    }

    pub(crate) async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, Box<dyn Error>> {
        log::info!("resolving {host} ...");
        let response = self.resolver.lookup_ip(host).await?;
        for ip in response.iter() {
            log::info!("{host}: adding ip {ip}");
        }
        Ok(response.iter().collect())
    }
}
