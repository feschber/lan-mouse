use clap::{Args, Parser, Subcommand};
use futures::StreamExt;

use std::{
    net::IpAddr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use thiserror::Error;

use lan_mouse_ipc::{
    AsyncFrontendEventReader, AsyncFrontendRequestWriter, ClientHandle, ConnectionError,
    DiscoveredDevice, FrontendEvent, FrontendRequest, IpcError, Position, connect_async,
};

#[derive(Debug, Error)]
pub enum CliError {
    /// is the service running?
    #[error("could not connect: `{0}` - is the service running?")]
    ServiceNotRunning(#[from] ConnectionError),
    #[error("error communicating with service: {0}")]
    Ipc(#[from] IpcError),
}

#[derive(Parser, Clone, Debug, PartialEq, Eq)]
#[command(name = "lan-mouse-cli", about = "LanMouse CLI interface")]
pub struct CliArgs {
    #[command(subcommand)]
    command: CliSubcommand,
}

#[derive(Args, Clone, Debug, PartialEq, Eq)]
struct Client {
    #[arg(long)]
    hostname: Option<String>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    ips: Option<Vec<IpAddr>>,
    #[arg(long)]
    enter_hook: Option<String>,
    #[arg(long)]
    leave_hook: Option<String>,
}

#[derive(Clone, Subcommand, Debug, PartialEq, Eq)]
enum CliSubcommand {
    /// add a new client
    AddClient(Client),
    /// remove an existing client
    RemoveClient { id: ClientHandle },
    /// activate a client
    Activate { id: ClientHandle },
    /// deactivate a client
    Deactivate { id: ClientHandle },
    /// list configured clients
    List,
    /// change hostname
    SetHost {
        id: ClientHandle,
        host: Option<String>,
    },
    /// change port
    SetPort { id: ClientHandle, port: u16 },
    /// set position
    SetPosition { id: ClientHandle, pos: Position },
    /// set ips
    SetIps { id: ClientHandle, ips: Vec<IpAddr> },
    /// re-enable capture
    EnableCapture,
    /// re-enable emulation
    EnableEmulation,
    /// authorize a public key
    AuthorizeKey {
        description: String,
        sha256_fingerprint: String,
    },
    /// deauthorize a public key
    RemoveAuthorizedKey { sha256_fingerprint: String },
    /// save configuration to file
    SaveConfig,
    /// list Lan Mouse devices found on the local network
    Discover,
    /// add and activate a client for a device listed by `discover`
    AddDiscovered {
        /// name, hostname or id of the device
        device: String,
        /// side of this screen the device is on
        pos: Position,
    },
    /// allow a device listed by `discover` to control this device
    AuthorizeDiscovered {
        /// name, hostname or id of the device
        device: String,
    },
}

pub async fn run(args: CliArgs) -> Result<(), CliError> {
    execute(args.command).await?;
    Ok(())
}

async fn execute(cmd: CliSubcommand) -> Result<(), CliError> {
    let (mut rx, mut tx) = connect_async(Some(Duration::from_millis(500))).await?;
    match cmd {
        CliSubcommand::AddClient(Client {
            hostname,
            port,
            ips,
            enter_hook,
            leave_hook,
        }) => {
            tx.request(FrontendRequest::Create).await?;
            while let Some(e) = rx.next().await {
                if let FrontendEvent::Created(handle, _, _) = e? {
                    if let Some(hostname) = hostname {
                        tx.request(FrontendRequest::UpdateHostname(handle, Some(hostname)))
                            .await?;
                    }
                    if let Some(port) = port {
                        tx.request(FrontendRequest::UpdatePort(handle, port))
                            .await?;
                    }
                    if let Some(ips) = ips {
                        tx.request(FrontendRequest::UpdateFixIps(handle, ips))
                            .await?;
                    }
                    if let Some(enter_hook) = enter_hook {
                        tx.request(FrontendRequest::UpdateEnterHook(handle, Some(enter_hook)))
                            .await?;
                    }
                    if let Some(leave_hook) = leave_hook {
                        tx.request(FrontendRequest::UpdateLeaveHook(handle, Some(leave_hook)))
                            .await?;
                    }
                    break;
                }
            }
        }
        CliSubcommand::RemoveClient { id } => tx.request(FrontendRequest::Delete(id)).await?,
        CliSubcommand::Activate { id } => tx.request(FrontendRequest::Activate(id, true)).await?,
        CliSubcommand::Deactivate { id } => {
            tx.request(FrontendRequest::Activate(id, false)).await?
        }
        CliSubcommand::List => {
            tx.request(FrontendRequest::Enumerate()).await?;
            while let Some(e) = rx.next().await {
                if let FrontendEvent::Enumerate(clients) = e? {
                    for (handle, config, state) in clients {
                        let host = config.hostname.unwrap_or("unknown".to_owned());
                        let port = config.port;
                        let pos = config.pos;
                        let active = state.active;
                        let ips = state.ips;
                        println!(
                            "id {handle}: {host}:{port} ({pos}) active: {active}, ips: {ips:?}"
                        );
                    }
                    break;
                }
            }
        }
        CliSubcommand::SetHost { id, host } => {
            tx.request(FrontendRequest::UpdateHostname(id, host))
                .await?
        }
        CliSubcommand::SetPort { id, port } => {
            tx.request(FrontendRequest::UpdatePort(id, port)).await?
        }
        CliSubcommand::SetPosition { id, pos } => {
            tx.request(FrontendRequest::UpdatePosition(id, pos)).await?
        }
        CliSubcommand::SetIps { id, ips } => {
            tx.request(FrontendRequest::UpdateFixIps(id, ips)).await?
        }
        CliSubcommand::EnableCapture => tx.request(FrontendRequest::EnableCapture).await?,
        CliSubcommand::EnableEmulation => tx.request(FrontendRequest::EnableEmulation).await?,
        CliSubcommand::AuthorizeKey {
            description,
            sha256_fingerprint,
        } => {
            tx.request(FrontendRequest::AuthorizeKey(
                description,
                sha256_fingerprint,
            ))
            .await?
        }
        CliSubcommand::RemoveAuthorizedKey { sha256_fingerprint } => {
            tx.request(FrontendRequest::RemoveAuthorizedKey(sha256_fingerprint))
                .await?
        }
        CliSubcommand::SaveConfig => tx.request(FrontendRequest::SaveConfiguration).await?,
        CliSubcommand::Discover => {
            let devices = discovered(&mut rx, &mut tx).await?;
            if devices.is_empty() {
                println!("no devices found");
            }
            for device in devices {
                let os = device.os.as_deref().unwrap_or("unknown");
                let ips = device
                    .ips
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                println!(
                    "{} ({os}): {}, port {} [{ips}]",
                    device.name, device.hostname, device.port
                );
                if let Some(fingerprint) = device.fingerprint {
                    println!("    fingerprint: {fingerprint}");
                }
                println!("    id: {}", device.id);
            }
        }
        CliSubcommand::AddDiscovered { device, pos } => {
            let devices = discovered(&mut rx, &mut tx).await?;
            if let Some(d) = find_device(&devices, &device) {
                tx.request(FrontendRequest::AddDiscovered(d.id.clone(), pos))
                    .await?;
                println!("added {} ({pos})", d.name);
            }
        }
        CliSubcommand::AuthorizeDiscovered { device } => {
            let devices = discovered(&mut rx, &mut tx).await?;
            if let Some(d) = find_device(&devices, &device) {
                match &d.fingerprint {
                    Some(fingerprint) => {
                        tx.request(FrontendRequest::AuthorizeKey(
                            d.name.clone(),
                            fingerprint.clone(),
                        ))
                        .await?;
                        println!("authorized {} ({fingerprint})", d.name);
                    }
                    None => eprintln!("{} did not announce a fingerprint", d.name),
                }
            }
        }
    }
    wait_until_handled(&mut rx, &mut tx).await
}

/// Wait until the service has handled every request sent so far.
/// Exiting right after a request can lose it: on Windows, closing a
/// connection with unread data resets it, and the service then drops
/// the requests it has not read yet.
async fn wait_until_handled(
    rx: &mut AsyncFrontendEventReader,
    tx: &mut AsyncFrontendRequestWriter,
) -> Result<(), CliError> {
    let token = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or_default()
        ^ u64::from(std::process::id());
    tx.request(FrontendRequest::Ping(token)).await?;
    while let Some(e) = rx.next().await {
        if let FrontendEvent::Pong(t) = e? {
            if t == token {
                break;
            }
        }
    }
    Ok(())
}

/// the discovered device matching `query` by name, hostname or id
fn find_device<'a>(devices: &'a [DiscoveredDevice], query: &str) -> Option<&'a DiscoveredDevice> {
    let found = devices
        .iter()
        .filter(|d| {
            [&d.id, &d.name, &d.hostname]
                .iter()
                .any(|s| s.eq_ignore_ascii_case(query))
        })
        .collect::<Vec<_>>();
    match found.as_slice() {
        [d] => Some(d),
        [] => {
            eprintln!("no device \"{query}\" found, see `discover`");
            None
        }
        _ => {
            eprintln!("\"{query}\" matches more than one device, use its id");
            None
        }
    }
}

/// devices the service currently sees on the local network
async fn discovered(
    rx: &mut AsyncFrontendEventReader,
    tx: &mut AsyncFrontendRequestWriter,
) -> Result<Vec<DiscoveredDevice>, CliError> {
    tx.request(FrontendRequest::EnumerateDiscovered).await?;
    while let Some(e) = rx.next().await {
        if let FrontendEvent::Discovered(devices) = e? {
            return Ok(devices);
        }
    }
    Ok(vec![])
}
