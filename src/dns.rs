use anyhow::Result;
use std::{error::Error, net::IpAddr};

use trust_dns_resolver::{TokioAsyncResolver, config::{ResolverConfig, ResolverOpts}};

pub(crate) struct DnsResolver {
    resolver: TokioAsyncResolver,
}
impl DnsResolver {
    pub(crate) async fn new() -> Result<Self> {
        let resolver = TokioAsyncResolver::tokio(
            ResolverConfig::default(),
            ResolverOpts::default(),
        );
        Ok(Self { resolver })
    }

    pub(crate) async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, Box<dyn Error>> {
        log::info!("resolving {host} ...");
        let response = self.resolver.lookup_ip(host).await?;
        Ok(response.iter().collect())
    }
}
