//! Automatic discovery of other Lan Mouse devices on the local network.
//!
//! Every instance advertises itself as a `_lan-mouse._udp` DNS-SD service via
//! multicast DNS (Bonjour) and browses for other instances. The service uses
//! the results to list nearby devices and to keep the addresses of configured
//! clients up to date when they change.

use std::{io, net::IpAddr, time::Duration};

use lan_mouse_ipc::DiscoveredDevice;
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use thiserror::Error;

const SERVICE_TYPE: &str = "_lan-mouse._udp.local.";

const TXT_NAME: &str = "name";
const TXT_FINGERPRINT: &str = "fp";
const TXT_OS: &str = "os";

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("mDNS error: `{0}`")]
    Mdns(#[from] mdns_sd::Error),
    #[error("could not determine hostname: `{0}`")]
    Hostname(#[from] io::Error),
}

pub(crate) enum DiscoveryEvent {
    /// a device was found or its information changed
    Resolved(DiscoveredDevice),
    /// the device with the given id is no longer available
    Removed(String),
}

pub(crate) struct Discovery {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<ServiceEvent>,
    instance_name: String,
    host_name: String,
    properties: Vec<(&'static str, String)>,
    /// current advertisement of this device
    service_info: ServiceInfo,
}

impl Discovery {
    /// start advertising this device on `port` and browsing for others
    pub(crate) fn new(port: u16, fingerprint: &str) -> Result<Self, DiscoveryError> {
        let name = device_name()?;
        // the fingerprint suffix keeps the instance name unique, even if two
        // devices share a hostname or one device runs multiple instances
        let short_fingerprint: String = fingerprint.chars().filter(|&c| c != ':').take(8).collect();
        let instance_name = format!("{name}-{short_fingerprint}");
        let host_name = format!("{}.local.", host_label(&name));
        let properties = vec![
            (TXT_NAME, name),
            (TXT_FINGERPRINT, fingerprint.to_owned()),
            (TXT_OS, std::env::consts::OS.to_owned()),
        ];
        let service_info = advertisement(&instance_name, &host_name, &properties, port)?;

        let daemon = ServiceDaemon::new()?;
        daemon.register(service_info.clone())?;
        let events = daemon.browse(SERVICE_TYPE)?;
        log::info!("advertising as \"{instance_name}\" ({host_name}) on port {port} via mDNS");

        Ok(Self {
            daemon,
            events,
            instance_name,
            host_name,
            properties,
            service_info,
        })
    }

    /// re-announce this device with a new port
    pub(crate) fn set_port(&mut self, port: u16) {
        if self.service_info.get_port() == port {
            return;
        }
        let service_info =
            match advertisement(&self.instance_name, &self.host_name, &self.properties, port) {
                Ok(info) => info,
                Err(e) => return log::warn!("could not update mDNS advertisement: {e}"),
            };
        let _ = self.daemon.unregister(self.service_info.get_fullname());
        if let Err(e) = self.daemon.register(service_info.clone()) {
            log::warn!("could not update mDNS advertisement: {e}");
        }
        self.service_info = service_info;
    }

    /// next change in the set of discovered devices
    pub(crate) async fn event(&mut self) -> DiscoveryEvent {
        loop {
            let event = match self.events.recv_async().await {
                Ok(event) => event,
                // the mDNS daemon is gone, there will be no more events
                Err(_) => std::future::pending().await,
            };
            match event {
                ServiceEvent::ServiceResolved(service) => {
                    if service.get_fullname() == self.service_info.get_fullname() {
                        continue;
                    }
                    if let Some(device) = to_device(&service) {
                        return DiscoveryEvent::Resolved(device);
                    }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    return DiscoveryEvent::Removed(fullname);
                }
                _ => {}
            }
        }
    }

    /// withdraw the advertisement and stop the mDNS daemon
    pub(crate) async fn terminate(&mut self) {
        // unregistering sends a goodbye packet, so peers drop this device right away
        if let Ok(status) = self.daemon.unregister(self.service_info.get_fullname()) {
            let _ = tokio::time::timeout(Duration::from_secs(1), status.recv_async()).await;
        }
        let _ = self.daemon.shutdown();
    }
}

fn advertisement(
    instance_name: &str,
    host_name: &str,
    properties: &[(&'static str, String)],
    port: u16,
) -> Result<ServiceInfo, mdns_sd::Error> {
    // no addresses given: `enable_addr_auto` advertises all of this
    // device's addresses and follows changes to them
    Ok(
        ServiceInfo::new(SERVICE_TYPE, instance_name, host_name, (), port, properties)?
            .enable_addr_auto(),
    )
}

fn to_device(service: &ResolvedService) -> Option<DiscoveredDevice> {
    let mut ips = service
        .get_addresses()
        .iter()
        .map(|ip| ip.to_ip_addr())
        .filter(is_reachable)
        .collect::<Vec<_>>();
    if ips.is_empty() {
        return None;
    }
    ips.sort();
    let hostname = service.get_hostname().trim_end_matches('.').to_owned();
    let name = service
        .get_property_val_str(TXT_NAME)
        .map(str::to_owned)
        .unwrap_or_else(|| hostname.clone());
    Some(DiscoveredDevice {
        id: service.get_fullname().to_owned(),
        name,
        hostname,
        ips,
        port: service.get_port(),
        fingerprint: service
            .get_property_val_str(TXT_FINGERPRINT)
            .map(str::to_owned),
        os: service.get_property_val_str(TXT_OS).map(str::to_owned),
    })
}

/// IPv6 link-local addresses need a scope id to connect to, which
/// `IpAddr` can't carry, so only keep addresses we can actually dial
fn is_reachable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !ip.is_loopback() && !ip.is_unspecified(),
        IpAddr::V6(ip) => !ip.is_loopback() && !ip.is_unspecified() && !ip.is_unicast_link_local(),
    }
}

/// short hostname of this device, without any domain
fn device_name() -> Result<String, io::Error> {
    let hostname = hostname::get()?.to_string_lossy().into_owned();
    let name = hostname.split('.').next().unwrap_or_default();
    Ok(if name.is_empty() {
        "lan-mouse".to_owned()
    } else {
        name.to_owned()
    })
}

/// `name` as a valid DNS label
fn host_label(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// whether `hostname`, as configured for a client, refers to `device`
pub(crate) fn matches_hostname(device: &DiscoveredDevice, hostname: &str) -> bool {
    let hostname = normalize_hostname(hostname);
    hostname == normalize_hostname(&device.hostname) || hostname == normalize_hostname(&device.name)
}

/// lowercase hostname without trailing dot and `.local` suffix
fn normalize_hostname(hostname: &str) -> String {
    let hostname = hostname.trim_end_matches('.').to_lowercase();
    match hostname.strip_suffix(".local") {
        Some(hostname) => hostname.to_owned(),
        None => hostname,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, hostname: &str) -> DiscoveredDevice {
        DiscoveredDevice {
            id: format!("{name}-12345678.{SERVICE_TYPE}"),
            name: name.to_owned(),
            hostname: hostname.to_owned(),
            ips: vec![],
            port: 4242,
            fingerprint: None,
            os: None,
        }
    }

    #[test]
    fn hostname_matching() {
        let mac = device("Office-MacBook-Pro", "Office-MacBook-Pro.local");
        assert!(matches_hostname(&mac, "Office-MacBook-Pro.local"));
        assert!(matches_hostname(&mac, "office-macbook-pro.local."));
        assert!(matches_hostname(&mac, "Office-MacBook-Pro"));
        assert!(!matches_hostname(&mac, "Office-MacBook"));
        assert!(!matches_hostname(&mac, "192.168.1.20"));

        let pc = device("DESKTOP 1", "DESKTOP-1.local");
        assert!(matches_hostname(&pc, "desktop-1"));
        assert!(matches_hostname(&pc, "DESKTOP 1"));
    }

    #[test]
    fn unreachable_addresses() {
        assert!(is_reachable(&"192.168.1.20".parse().unwrap()));
        assert!(is_reachable(&"fd00::20".parse().unwrap()));
        assert!(!is_reachable(&"127.0.0.1".parse().unwrap()));
        assert!(!is_reachable(&"fe80::1".parse().unwrap()));
        assert!(!is_reachable(&"::1".parse().unwrap()));
    }

    #[test]
    fn host_labels() {
        assert_eq!(host_label("DESKTOP-1"), "DESKTOP-1");
        assert_eq!(host_label("Sam's PC"), "Sam-s-PC");
    }

    /// two instances on the same machine find each other over multicast loopback
    #[tokio::test]
    #[ignore = "needs a network interface with multicast"]
    async fn discovers_peer() {
        let fp_a = "aa:aa:aa:aa:aa:aa:aa:aa";
        let fp_b = "bb:bb:bb:bb:bb:bb:bb:bb";
        let mut a = Discovery::new(4300, fp_a).expect("discovery a");
        let mut b = Discovery::new(4301, fp_b).expect("discovery b");
        let found = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let DiscoveryEvent::Resolved(device) = a.event().await {
                    if device.fingerprint.as_deref() == Some(fp_b) {
                        return device;
                    }
                }
            }
        })
        .await
        .expect("peer not discovered within 10s");
        assert_eq!(found.port, 4301);
        assert!(!found.ips.is_empty());
        a.terminate().await;
        b.terminate().await;
    }
}
