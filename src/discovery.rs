//! Automatic discovery of other Lan Mouse devices on the local network.
//!
//! Devices find each other in two ways, so discovery still works on networks
//! that filter one of them:
//! - mDNS (Bonjour): every instance advertises itself as a `_lan-mouse._udp`
//!   DNS-SD service and browses for other instances.
//! - Broadcast beacons: every instance broadcasts a small JSON message on UDP
//!   port [`BEACON_PORT`] every few seconds and answers beacons of devices it
//!   has not seen yet. Some routers drop multicast between Wi-Fi and Ethernet
//!   but still forward broadcasts, which ARP depends on.
//!
//! What both methods report is merged per device. The service uses the result
//! to list nearby devices and to keep the addresses of configured clients up
//! to date when they change.

use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::{Duration, Instant},
};

use if_addrs::{IfAddr, IfOperStatus};
use lan_mouse_ipc::DiscoveredDevice;
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use thiserror::Error;
use tokio::{
    net::UdpSocket,
    time::{Interval, MissedTickBehavior},
};

const SERVICE_TYPE: &str = "_lan-mouse._udp.local.";

const TXT_NAME: &str = "name";
const TXT_FINGERPRINT: &str = "fp";
const TXT_OS: &str = "os";

/// UDP port broadcast beacons are sent to and received on
const BEACON_PORT: u16 = 4241;
/// how often this device broadcasts its beacon
const BEACON_INTERVAL: Duration = Duration::from_secs(5);
/// a device only seen via beacons is dropped after this long without one
const BEACON_TIMEOUT: Duration = Duration::from_secs(16);
/// version of the beacon message format
const BEACON_VERSION: u8 = 1;

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("mDNS error: `{0}`")]
    Mdns(#[from] mdns_sd::Error),
    #[error("broadcast discovery error: `{0}`")]
    Beacon(io::Error),
    #[error("could not determine hostname: `{0}`")]
    Hostname(io::Error),
}

pub(crate) enum DiscoveryEvent {
    /// a device was found or its information changed
    Resolved(DiscoveredDevice),
    /// the device with the given id is no longer available
    Removed(String),
}

/// this device, as announced to others
struct LocalDevice {
    /// DNS-SD instance name, unique per device
    instance_name: String,
    /// id other devices know this one by (lowercase DNS-SD full name)
    id: String,
    /// human readable device name
    name: String,
    /// mDNS hostname with trailing dot, e.g. `my-pc.local.`
    host_name: String,
    fingerprint: String,
    port: u16,
}

impl LocalDevice {
    fn beacon(&self, bye: bool) -> Vec<u8> {
        let beacon = Beacon {
            lan_mouse: BEACON_VERSION,
            id: self.id.clone(),
            name: self.name.clone(),
            hostname: self.host_name.trim_end_matches('.').to_owned(),
            fingerprint: self.fingerprint.clone(),
            os: std::env::consts::OS.to_owned(),
            port: self.port,
            bye,
        };
        serde_json::to_vec(&beacon).expect("beacon serialization cannot fail")
    }
}

/// broadcast beacon message
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Beacon {
    /// format version, also marks the message as a Lan Mouse beacon
    lan_mouse: u8,
    id: String,
    name: String,
    hostname: String,
    fingerprint: String,
    os: String,
    port: u16,
    /// the sender is shutting down
    #[serde(default)]
    bye: bool,
}

struct Mdns {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<ServiceEvent>,
    /// current advertisement of this device
    service_info: ServiceInfo,
}

impl Mdns {
    fn new(local: &LocalDevice) -> Result<Self, mdns_sd::Error> {
        let service_info = advertisement(local)?;
        let daemon = ServiceDaemon::new()?;
        daemon.register(service_info.clone())?;
        let events = daemon.browse(SERVICE_TYPE)?;
        Ok(Self {
            daemon,
            events,
            service_info,
        })
    }

    /// replace the advertisement, e.g. after a port change
    fn readvertise(&mut self, local: &LocalDevice) {
        let service_info = match advertisement(local) {
            Ok(info) => info,
            Err(e) => return log::warn!("could not update mDNS advertisement: {e}"),
        };
        let _ = self.daemon.unregister(self.service_info.get_fullname());
        if let Err(e) = self.daemon.register(service_info.clone()) {
            log::warn!("could not update mDNS advertisement: {e}");
        }
        self.service_info = service_info;
    }

    async fn terminate(&mut self) {
        // unregistering sends a goodbye packet, so peers drop this device right away
        if let Ok(status) = self.daemon.unregister(self.service_info.get_fullname()) {
            let _ = tokio::time::timeout(Duration::from_secs(1), status.recv_async()).await;
        }
        let _ = self.daemon.shutdown();
    }
}

/// what each discovery method last reported about a device
#[derive(Default)]
struct Found {
    mdns: Option<DiscoveredDevice>,
    beacon: Option<(DiscoveredDevice, Instant)>,
}

impl Found {
    /// combined view of both methods: mDNS details if available, addresses from both
    fn merged(&self) -> Option<DiscoveredDevice> {
        let beacon = self.beacon.as_ref().map(|(device, _)| device);
        let mut device = self.mdns.as_ref().or(beacon)?.clone();
        device.ips = self
            .mdns
            .iter()
            .chain(beacon)
            .flat_map(|d| d.ips.iter().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Some(device)
    }
}

pub(crate) struct Discovery {
    local: LocalDevice,
    mdns: Option<Mdns>,
    beacon_socket: Option<UdpSocket>,
    beacon_timer: Interval,
    /// devices seen by any method, by id
    found: HashMap<String, Found>,
    /// events not yet returned by [`Discovery::event`]
    pending: VecDeque<DiscoveryEvent>,
}

impl Discovery {
    /// start announcing this device on `port` and looking for others
    pub(crate) fn new(port: u16, fingerprint: &str) -> Result<Self, DiscoveryError> {
        Self::with_methods(port, fingerprint, true, true)
    }

    fn with_methods(
        port: u16,
        fingerprint: &str,
        use_mdns: bool,
        use_beacon: bool,
    ) -> Result<Self, DiscoveryError> {
        let name = device_name().map_err(DiscoveryError::Hostname)?;
        // the fingerprint suffix keeps the instance name unique, even if two
        // devices share a hostname or one device runs multiple instances
        let short_fingerprint: String = fingerprint.chars().filter(|&c| c != ':').take(8).collect();
        let instance_name = format!("{name}-{short_fingerprint}");
        let local = LocalDevice {
            id: format!("{instance_name}.{SERVICE_TYPE}").to_lowercase(),
            host_name: format!("{}.local.", host_label(&name)),
            instance_name,
            name,
            fingerprint: fingerprint.to_owned(),
            port,
        };

        let mut errors = vec![];
        let mdns = if use_mdns {
            match Mdns::new(&local) {
                Ok(mdns) => {
                    log::info!(
                        "advertising as \"{}\" ({}) on port {port} via mDNS",
                        local.instance_name,
                        local.host_name
                    );
                    Some(mdns)
                }
                Err(e) => {
                    errors.push(DiscoveryError::Mdns(e));
                    None
                }
            }
        } else {
            None
        };
        let beacon_socket = if use_beacon {
            match beacon_socket() {
                Ok(socket) => {
                    log::info!(
                        "announcing \"{}\" via broadcast on UDP port {BEACON_PORT}",
                        local.instance_name
                    );
                    Some(socket)
                }
                Err(e) => {
                    errors.push(DiscoveryError::Beacon(e));
                    None
                }
            }
        } else {
            None
        };
        if mdns.is_none() && beacon_socket.is_none() {
            return Err(errors.remove(0));
        }
        for e in errors {
            log::warn!("{e}");
        }

        // the first tick fires right away, announcing this device on startup
        let mut beacon_timer = tokio::time::interval(BEACON_INTERVAL);
        beacon_timer.set_missed_tick_behavior(MissedTickBehavior::Delay);

        Ok(Self {
            local,
            mdns,
            beacon_socket,
            beacon_timer,
            found: Default::default(),
            pending: Default::default(),
        })
    }

    /// re-announce this device with a new port
    pub(crate) fn set_port(&mut self, port: u16) {
        if self.local.port == port {
            return;
        }
        self.local.port = port;
        if let Some(mdns) = &mut self.mdns {
            mdns.readvertise(&self.local);
        }
        self.broadcast_beacon(false);
    }

    /// next change in the set of discovered devices
    ///
    /// Cancel safe: state only changes after one of the awaited sources is ready.
    pub(crate) async fn event(&mut self) -> DiscoveryEvent {
        loop {
            if let Some(event) = self.pending.pop_front() {
                return event;
            }
            tokio::select! {
                event = next_mdns_event(&self.mdns) => self.handle_mdns_event(event),
                packet = next_beacon(&self.beacon_socket) => self.handle_beacon(packet),
                _ = self.beacon_timer.tick() => self.handle_timer(),
            }
        }
    }

    /// say goodbye to other devices and stop announcing this one
    pub(crate) async fn terminate(&mut self) {
        self.broadcast_beacon(true);
        if let Some(mdns) = &mut self.mdns {
            mdns.terminate().await;
        }
    }

    fn handle_mdns_event(&mut self, event: ServiceEvent) {
        match event {
            ServiceEvent::ServiceResolved(service) => {
                let Some(device) = to_device(&service) else {
                    return;
                };
                if device.id == self.local.id {
                    return;
                }
                let id = device.id.clone();
                self.found.entry(id.clone()).or_default().mdns = Some(device);
                self.update(&id);
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                let id = fullname.to_lowercase();
                if let Some(found) = self.found.get_mut(&id) {
                    found.mdns = None;
                    self.update(&id);
                }
            }
            _ => {}
        }
    }

    fn handle_beacon(&mut self, packet: io::Result<(Vec<u8>, SocketAddr)>) {
        let (data, addr) = match packet {
            Ok(packet) => packet,
            // e.g. Windows reports ICMP port-unreachable for an earlier reply here
            Err(e) => return log::debug!("beacon receive error: {e}"),
        };
        let Ok(beacon) = serde_json::from_slice::<Beacon>(&data) else {
            return;
        };
        let id = beacon.id.to_lowercase();
        if beacon.lan_mouse != BEACON_VERSION || id == self.local.id || !is_reachable(&addr.ip()) {
            return;
        }
        if beacon.bye {
            if let Some(found) = self.found.get_mut(&id) {
                found.beacon = None;
                self.update(&id);
            }
            return;
        }
        let device = DiscoveredDevice {
            id: id.clone(),
            name: beacon.name,
            hostname: beacon.hostname,
            // the address the beacon came from is reachable from here by definition
            ips: vec![addr.ip()],
            port: beacon.port,
            fingerprint: Some(beacon.fingerprint),
            os: Some(beacon.os),
        };
        let found = self.found.entry(id.clone()).or_default();
        let is_new = found.beacon.is_none();
        found.beacon = Some((device, Instant::now()));
        // answer new devices directly, so they don't wait for our next broadcast
        if is_new {
            self.send_beacon(addr, false);
        }
        self.update(&id);
    }

    fn handle_timer(&mut self) {
        self.broadcast_beacon(false);
        let now = Instant::now();
        let mut expired = vec![];
        for (id, found) in self.found.iter_mut() {
            if found
                .beacon
                .as_ref()
                .is_some_and(|(_, seen)| now.duration_since(*seen) > BEACON_TIMEOUT)
            {
                found.beacon = None;
                expired.push(id.clone());
            }
        }
        for id in expired {
            self.update(&id);
        }
    }

    /// report the combined view of all methods on device `id`
    fn update(&mut self, id: &str) {
        match self.found.get(id).and_then(Found::merged) {
            Some(device) => self.pending.push_back(DiscoveryEvent::Resolved(device)),
            None => {
                if self.found.remove(id).is_some() {
                    self.pending
                        .push_back(DiscoveryEvent::Removed(id.to_owned()));
                }
            }
        }
    }

    fn send_beacon(&self, to: SocketAddr, bye: bool) {
        let Some(socket) = &self.beacon_socket else {
            return;
        };
        // non-blocking send: nothing is lost if `event()` gets cancelled
        if let Err(e) = socket.try_send_to(&self.local.beacon(bye), to) {
            log::debug!("could not send beacon to {to}: {e}");
        }
    }

    fn broadcast_beacon(&self, bye: bool) {
        for addr in broadcast_addresses() {
            self.send_beacon(SocketAddr::new(addr.into(), BEACON_PORT), bye);
        }
    }
}

async fn next_mdns_event(mdns: &Option<Mdns>) -> ServiceEvent {
    match mdns {
        Some(mdns) => match mdns.events.recv_async().await {
            Ok(event) => event,
            // the mDNS daemon is gone, there will be no more events
            Err(_) => std::future::pending().await,
        },
        None => std::future::pending().await,
    }
}

async fn next_beacon(socket: &Option<UdpSocket>) -> io::Result<(Vec<u8>, SocketAddr)> {
    let Some(socket) = socket else {
        return std::future::pending().await;
    };
    let mut buf = [0u8; 2048];
    let (len, addr) = socket.recv_from(&mut buf).await?;
    Ok((buf[..len].to_vec(), addr))
}

/// UDP socket for sending and receiving beacons
fn beacon_socket() -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // let several instances on one machine receive broadcasts
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_broadcast(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, BEACON_PORT)).into())?;
    UdpSocket::from_std(socket.into())
}

/// directed broadcast address of every IPv4 network this device is on
fn broadcast_addresses() -> Vec<Ipv4Addr> {
    let interfaces = match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces,
        Err(e) => {
            log::debug!("could not list network interfaces: {e}");
            return vec![];
        }
    };
    let addrs = interfaces
        .into_iter()
        .filter(|i| !matches!(i.oper_status, IfOperStatus::Down | IfOperStatus::NotPresent))
        .filter_map(|i| match i.addr {
            IfAddr::V4(addr) => Some(addr),
            IfAddr::V6(_) => None,
        })
        // VPNs like Tailscale use single-address networks with nobody to broadcast to
        .filter(|addr| !addr.ip.is_loopback() && !addr.ip.is_link_local() && addr.prefixlen < 31)
        .map(|addr| Ipv4Addr::from(u32::from(addr.ip) | !u32::from(addr.netmask)))
        .collect::<BTreeSet<_>>();
    addrs.into_iter().collect()
}

fn advertisement(local: &LocalDevice) -> Result<ServiceInfo, mdns_sd::Error> {
    let properties = [
        (TXT_NAME, local.name.as_str()),
        (TXT_FINGERPRINT, local.fingerprint.as_str()),
        (TXT_OS, std::env::consts::OS),
    ];
    // no addresses given: `enable_addr_auto` advertises all of this
    // device's addresses and follows changes to them
    Ok(ServiceInfo::new(
        SERVICE_TYPE,
        &local.instance_name,
        &local.host_name,
        (),
        local.port,
        &properties[..],
    )?
    .enable_addr_auto())
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
        id: service.get_fullname().to_lowercase(),
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
            id: format!("{name}-12345678.{SERVICE_TYPE}").to_lowercase(),
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

    #[test]
    fn beacon_round_trip() {
        let local = LocalDevice {
            instance_name: "DeskPc-1a2b3c4d".to_owned(),
            id: "deskpc-1a2b3c4d._lan-mouse._udp.local.".to_owned(),
            name: "DeskPc".to_owned(),
            host_name: "DeskPc.local.".to_owned(),
            fingerprint: "1a:2b:3c:4d".to_owned(),
            port: 4242,
        };
        let beacon: Beacon = serde_json::from_slice(&local.beacon(false)).unwrap();
        assert_eq!(beacon.lan_mouse, BEACON_VERSION);
        assert_eq!(beacon.id, local.id);
        assert_eq!(beacon.hostname, "DeskPc.local");
        assert_eq!(beacon.port, 4242);
        assert!(!beacon.bye);
        assert!(serde_json::from_slice::<Beacon>(b"{\"unrelated\": true}").is_err());
    }

    #[test]
    fn merges_methods() {
        let mut from_mdns = device("Mac", "Mac.local");
        from_mdns.ips = vec!["192.168.1.30".parse().unwrap(), "fd00::15".parse().unwrap()];
        from_mdns.os = Some("macos".to_owned());
        let mut from_beacon = device("Mac", "Mac.local");
        from_beacon.ips = vec![
            "10.0.0.15".parse().unwrap(),
            "192.168.1.30".parse().unwrap(),
        ];

        let mut found = Found::default();
        assert_eq!(found.merged(), None);

        found.beacon = Some((from_beacon.clone(), Instant::now()));
        assert_eq!(found.merged(), Some(from_beacon));

        found.mdns = Some(from_mdns.clone());
        let merged = found.merged().unwrap();
        assert_eq!(merged.os.as_deref(), Some("macos"));
        assert_eq!(merged.ips.len(), 3);

        found.beacon = None;
        assert_eq!(found.merged(), Some(from_mdns));
    }

    /// wait until `a` reports a device with fingerprint `fp`, while also driving `b`
    async fn discover(a: &mut Discovery, b: &mut Discovery, fp: &str) -> DiscoveredDevice {
        let wait = async {
            loop {
                tokio::select! {
                    event = a.event() => {
                        if let DiscoveryEvent::Resolved(device) = event {
                            if device.fingerprint.as_deref() == Some(fp) {
                                return device;
                            }
                        }
                    }
                    _ = b.event() => {}
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .expect("peer not discovered within 10s")
    }

    /// two instances on the same machine find each other via mDNS
    #[tokio::test]
    #[ignore = "needs a network interface with multicast"]
    async fn discovers_peer_via_mdns() {
        let (fp_a, fp_b) = ("aa:aa:aa:aa", "bb:bb:bb:bb");
        let mut a = Discovery::with_methods(4300, fp_a, true, false).expect("discovery a");
        let mut b = Discovery::with_methods(4301, fp_b, true, false).expect("discovery b");
        let found = discover(&mut a, &mut b, fp_b).await;
        assert_eq!(found.port, 4301);
        assert!(!found.ips.is_empty());
        a.terminate().await;
        b.terminate().await;
    }

    /// two instances on the same machine find each other via broadcast beacons
    #[tokio::test]
    #[ignore = "needs a network interface with broadcast"]
    async fn discovers_peer_via_beacon() {
        let (fp_a, fp_b) = ("cc:cc:cc:cc", "dd:dd:dd:dd");
        let mut a = Discovery::with_methods(4302, fp_a, false, true).expect("discovery a");
        let mut b = Discovery::with_methods(4303, fp_b, false, true).expect("discovery b");
        let found = discover(&mut a, &mut b, fp_b).await;
        assert_eq!(found.port, 4303);
        assert_eq!(found.ips.len(), 1);
        assert_eq!(found.os.as_deref(), Some(std::env::consts::OS));
        a.terminate().await;
        b.terminate().await;
    }
}
