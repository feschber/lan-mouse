use std::{error::Error, net::IpAddr};

use trust_dns_resolver::Resolver;

pub fn resolve(host: &str) -> Result<Vec<IpAddr>, Box<dyn Error>> {
    let response = Resolver::from_system_conf()?.lookup_ip(host)?;
    Ok(response.iter().collect())
}
