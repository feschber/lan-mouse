use anyhow::Result;
use std::{error::Error, net::IpAddr};

use trust_dns_resolver::Resolver;

pub(crate) struct DnsResolver {
    resolver: Resolver,
}
impl DnsResolver {
    pub(crate) fn new() -> Result<Self> {
        let resolver = Resolver::from_system_conf()?;
        Ok(Self { resolver })
    }

    pub(crate) fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, Box<dyn Error>> {
        log::info!("resolving {host} ...");
        let response = self.resolver.lookup_ip(host)?;
        Ok(response.iter().collect())
    }
}
