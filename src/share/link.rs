//! Reliable, encrypted links between devices for clipboard and file data.
//!
//! Input events keep using the DTLS/UDP channel. Clipboard contents and
//! files need reliable delivery and can be large, so they travel over TLS on
//! TCP (same port number as the UDP listener), authenticated with the same
//! certificates and authorized fingerprints.
//!
//! Every device listens and every device dials its configured clients. A
//! device that can't accept connections (e.g. behind a firewall it doesn't
//! control) is still reachable because the other side's link, dialed by it,
//! carries data both ways. If both directions succeed, the link dialed by the
//! device with the smaller fingerprint is kept.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    io,
    net::{IpAddr, SocketAddr},
    rc::Rc,
    sync::{Arc, RwLock},
    time::Duration,
};

use lan_mouse_ipc::DEFAULT_PORT;
use rustls::{
    DigitallySignedStruct, DistinguishedName, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream},
    sync::mpsc::{self, Receiver, Sender, UnboundedReceiver, UnboundedSender},
    task::spawn_local,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::sync::CancellationToken;
use webrtc_dtls::crypto::Certificate;

use super::proto::{Frame, Header, read_frame, write_frame};
use crate::{client::ClientManager, crypto};

const DIAL_INTERVAL: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const PING_INTERVAL: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(20);
/// frames of bulk data (file chunks) buffered per link
const BULK_QUEUE: usize = 32;
/// received frames waiting to be handled; full means backpressure on TCP
const EVENT_QUEUE: usize = 64;

pub(crate) type Fingerprint = String;

pub(crate) enum LinkEvent {
    Up(Fingerprint),
    Down(Fingerprint),
    Frame(Fingerprint, Frame),
}

/// Handle to send on one link.
#[derive(Clone)]
pub(crate) struct LinkSender {
    ctrl: UnboundedSender<Frame>,
    bulk: Sender<Frame>,
    cancel: CancellationToken,
}

impl LinkSender {
    /// small, urgent frames (clipboard, control); sent before bulk data
    pub(crate) fn send(&self, frame: Frame) {
        let _ = self.ctrl.send(frame);
    }
    /// bulk data with backpressure; fails once the link is gone
    pub(crate) async fn send_bulk(&self, frame: Frame) -> Result<(), ()> {
        tokio::select! {
            r = self.bulk.send(frame) => r.map_err(|_| ()),
            _ = self.cancel.cancelled() => Err(()),
        }
    }
}

struct Link {
    id: u64,
    peer_ip: IpAddr,
    dialer: Fingerprint,
    sender: LinkSender,
}

#[derive(Clone)]
pub(crate) struct Links {
    inner: Rc<RefCell<HashMap<Fingerprint, Link>>>,
}

impl Links {
    pub(crate) fn senders(&self) -> Vec<(Fingerprint, LinkSender)> {
        self.inner
            .borrow()
            .iter()
            .map(|(fp, l)| (fp.clone(), l.sender.clone()))
            .collect()
    }
    #[cfg(test)]
    pub(crate) fn get(&self, fp: &str) -> Option<LinkSender> {
        self.inner.borrow().get(fp).map(|l| l.sender.clone())
    }
    fn has_ip(&self, ip: IpAddr) -> bool {
        self.inner.borrow().values().any(|l| l.peer_ip == ip)
    }
}

#[derive(Debug)]
struct PeerVerifier {
    authorized: Arc<RwLock<HashMap<String, String>>>,
    provider: Arc<CryptoProvider>,
}

impl PeerVerifier {
    fn check(&self, cert: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        let fp = crypto::generate_fingerprint(cert.as_ref());
        if self.authorized.read().expect("lock").contains_key(&fp) {
            Ok(())
        } else {
            Err(rustls::Error::General(format!(
                "unauthorized fingerprint {fp}"
            )))
        }
    }
    fn tls12(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn tls13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ServerCertVerifier for PeerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.check(end_entity)?;
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.tls12(message, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.tls13(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

impl ClientCertVerifier for PeerVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self.check(end_entity)?;
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.tls12(message, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.tls13(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

fn tls_configs(
    cert: &Certificate,
    authorized: Arc<RwLock<HashMap<String, String>>>,
) -> Result<(TlsAcceptor, TlsConnector), rustls::Error> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PeerVerifier {
        authorized,
        provider: provider.clone(),
    });
    let certs: Vec<CertificateDer<'static>> = cert.certificate.clone();
    let key = || {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            cert.private_key.serialized_der.clone(),
        ))
    };
    let server = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(verifier.clone())
        .with_single_cert(certs.clone(), key())?;
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certs, key())?;
    Ok((
        TlsAcceptor::from(Arc::new(server)),
        TlsConnector::from(Arc::new(client)),
    ))
}

struct State {
    links: Links,
    my_fp: Fingerprint,
    next_id: RefCell<u64>,
    events: Sender<LinkEvent>,
    dialing: RefCell<HashSet<IpAddr>>,
}

/// Start listening and dialing. Returns the link table and the event stream.
pub(crate) async fn start(
    port: u16,
    cert: Certificate,
    authorized: Arc<RwLock<HashMap<String, String>>>,
    client_manager: ClientManager,
) -> io::Result<(Links, Receiver<LinkEvent>)> {
    let (acceptor, connector) = tls_configs(&cert, authorized).map_err(io::Error::other)?;
    let my_fp = crypto::certificate_fingerprint(&cert);
    let (events, events_rx) = mpsc::channel(EVENT_QUEUE);
    let links = Links {
        inner: Default::default(),
    };
    let state = Rc::new(State {
        links: links.clone(),
        my_fp,
        next_id: RefCell::new(0),
        events,
        dialing: Default::default(),
    });

    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    log::info!("data link listening on tcp port {port}");
    spawn_local(accept_loop(listener, acceptor, state.clone()));
    spawn_local(dial_loop(connector, state, client_manager));
    Ok((links, events_rx))
}

async fn accept_loop(listener: TcpListener, acceptor: TlsAcceptor, state: Rc<State>) {
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                log::warn!("data link accept: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let state = state.clone();
        spawn_local(async move {
            let _ = stream.set_nodelay(true);
            match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                Ok(Ok(tls)) => {
                    let fp = tls
                        .get_ref()
                        .1
                        .peer_certificates()
                        .and_then(|c| c.first())
                        .map(|c| crypto::generate_fingerprint(c.as_ref()));
                    if let Some(fp) = fp {
                        let dialer = fp.clone();
                        register(&state, tls, addr, fp, dialer);
                    }
                }
                Ok(Err(e)) => log::warn!("data link from {addr} rejected: {e}"),
                Err(_) => log::warn!("data link from {addr}: handshake timed out"),
            }
        });
    }
}

async fn dial_loop(connector: TlsConnector, state: Rc<State>, client_manager: ClientManager) {
    let mut interval = tokio::time::interval(DIAL_INTERVAL);
    loop {
        interval.tick().await;
        for handle in client_manager.registered_clients() {
            let Some(ips) = client_manager.get_ips(handle) else {
                continue;
            };
            let port = client_manager.get_port(handle).unwrap_or(DEFAULT_PORT);
            for ip in ips {
                if state.links.has_ip(ip) || state.dialing.borrow().contains(&ip) {
                    continue;
                }
                state.dialing.borrow_mut().insert(ip);
                let connector = connector.clone();
                let state = state.clone();
                spawn_local(async move {
                    dial(connector, &state, SocketAddr::new(ip, port)).await;
                    state.dialing.borrow_mut().remove(&ip);
                });
            }
        }
    }
}

async fn dial(connector: TlsConnector, state: &Rc<State>, addr: SocketAddr) {
    let stream = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            log::trace!("data link dial {addr}: {e}");
            return;
        }
        Err(_) => {
            log::trace!("data link dial {addr}: timed out");
            return;
        }
    };
    let _ = stream.set_nodelay(true);
    let name = ServerName::try_from("lan-mouse").expect("server name");
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, connector.connect(name, stream)).await {
        Ok(Ok(tls)) => {
            let fp = tls
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|c| c.first())
                .map(|c| crypto::generate_fingerprint(c.as_ref()));
            if let Some(fp) = fp {
                let dialer = state.my_fp.clone();
                register(state, tls, addr, fp, dialer);
            }
        }
        Ok(Err(e)) => log::warn!("data link to {addr} failed: {e}"),
        Err(_) => log::warn!("data link to {addr}: handshake timed out"),
    }
}

fn register<S>(state: &Rc<State>, stream: S, addr: SocketAddr, fp: Fingerprint, dialer: Fingerprint)
where
    S: AsyncRead + AsyncWrite + Unpin + 'static,
{
    if fp == state.my_fp {
        log::warn!("data link to {addr} reached ourselves, ignoring");
        return;
    }
    let preferred_dialer = std::cmp::min(&state.my_fp, &fp).clone();
    {
        let links = state.links.inner.borrow();
        if let Some(existing) = links.get(&fp) {
            let replace = dialer == preferred_dialer && existing.dialer != preferred_dialer;
            if !replace {
                log::debug!("data link to {addr}: already linked, dropping duplicate");
                return;
            }
        }
    }
    let id = {
        let mut next = state.next_id.borrow_mut();
        *next += 1;
        *next
    };
    let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
    let (bulk_tx, bulk_rx) = mpsc::channel(BULK_QUEUE);
    let cancel = CancellationToken::new();
    let sender = LinkSender {
        ctrl: ctrl_tx,
        bulk: bulk_tx,
        cancel: cancel.clone(),
    };
    let old = state.links.inner.borrow_mut().insert(
        fp.clone(),
        Link {
            id,
            peer_ip: addr.ip(),
            dialer,
            sender,
        },
    );
    if let Some(old) = old {
        old.sender.cancel.cancel();
    } else {
        let events = state.events.clone();
        let fp = fp.clone();
        spawn_local(async move {
            let _ = events.send(LinkEvent::Up(fp)).await;
        });
    }
    log::info!("data link up: {addr} ({})", short(&fp));

    let (reader, writer) = tokio::io::split(stream);
    spawn_local(write_loop(writer, ctrl_rx, bulk_rx, cancel.clone()));
    let state = state.clone();
    spawn_local(async move {
        read_loop(reader, &fp, &state.events, cancel.clone()).await;
        cancel.cancel();
        let removed = {
            let mut links = state.links.inner.borrow_mut();
            if links.get(&fp).is_some_and(|l| l.id == id) {
                links.remove(&fp);
                true
            } else {
                false
            }
        };
        if removed {
            log::info!("data link down: {addr} ({})", short(&fp));
            let _ = state.events.send(LinkEvent::Down(fp)).await;
        }
    });
}

async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut ctrl: UnboundedReceiver<Frame>,
    mut bulk: Receiver<Frame>,
    cancel: CancellationToken,
) {
    let mut ping = tokio::time::interval(PING_INTERVAL);
    loop {
        let frame = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            f = ctrl.recv() => match f { Some(f) => f, None => break },
            f = bulk.recv() => match f { Some(f) => f, None => break },
            _ = ping.tick() => Frame::new(Header::Ping),
        };
        if let Err(e) = write_frame(&mut writer, &frame).await {
            log::warn!("data link write: {e}");
            break;
        }
    }
    cancel.cancel();
    let _ = tokio::io::AsyncWriteExt::shutdown(&mut writer).await;
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: R,
    fp: &str,
    events: &Sender<LinkEvent>,
    cancel: CancellationToken,
) {
    loop {
        let frame = tokio::select! {
            _ = cancel.cancelled() => return,
            r = tokio::time::timeout(READ_TIMEOUT, read_frame(&mut reader)) => r,
        };
        match frame {
            Ok(Ok(frame)) => {
                if matches!(frame.header, Header::Ping) {
                    continue;
                }
                if events
                    .send(LinkEvent::Frame(fp.to_owned(), frame))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Ok(Err(e)) => {
                if e.kind() != io::ErrorKind::UnexpectedEof {
                    log::warn!("data link read: {e}");
                }
                return;
            }
            Err(_) => {
                log::warn!("data link: peer silent for {READ_TIMEOUT:?}");
                return;
            }
        }
    }
}

pub(crate) fn short(fp: &str) -> &str {
    &fp[..fp.len().min(11)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn peer(other_port: u16) -> (Certificate, ClientManager) {
        let cert = Certificate::generate_self_signed(["ignored".to_owned()]).unwrap();
        let cm = ClientManager::default();
        let h = cm.add_client();
        cm.set_fix_ips(h, vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
        cm.set_port(h, other_port);
        (cert, cm)
    }

    async fn next_frame(rx: &mut Receiver<LinkEvent>) -> (Fingerprint, Frame) {
        loop {
            match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await {
                Ok(Some(LinkEvent::Frame(fp, f))) => return (fp, f),
                Ok(Some(_)) => continue,
                other => panic!("no frame: {}", other.is_ok()),
            }
        }
    }

    #[tokio::test]
    async fn two_peers_link_once_and_exchange() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (pa, pb) = (34711, 34712);
                let (ca, cma) = peer(pb);
                let (cb, cmb) = peer(pa);
                let fa = crypto::certificate_fingerprint(&ca);
                let fb = crypto::certificate_fingerprint(&cb);
                let auth_a = Arc::new(RwLock::new(HashMap::from([(fb.clone(), "b".into())])));
                let auth_b = Arc::new(RwLock::new(HashMap::from([(fa.clone(), "a".into())])));
                let (la, mut ra) = start(pa, ca, auth_a, cma).await.unwrap();
                let (lb, mut rb) = start(pb, cb, auth_b, cmb).await.unwrap();

                // both dial each other; wait until both directions settled
                tokio::time::sleep(DIAL_INTERVAL * 2).await;
                assert_eq!(la.senders().len(), 1);
                assert_eq!(lb.senders().len(), 1);

                la.get(&fb).unwrap().send(Frame::with_body(
                    Header::FilesEnd { batch: 7 },
                    b"x".to_vec(),
                ));
                let (from, f) = next_frame(&mut rb).await;
                assert_eq!(from, fa);
                assert!(matches!(f.header, Header::FilesEnd { batch: 7 }));

                lb.get(&fa)
                    .unwrap()
                    .send_bulk(Frame::new(Header::FilesEnd { batch: 8 }))
                    .await
                    .unwrap();
                let (from, f) = next_frame(&mut ra).await;
                assert_eq!(from, fb);
                assert!(matches!(f.header, Header::FilesEnd { batch: 8 }));
            })
            .await;
    }

    #[tokio::test]
    async fn unauthorized_peer_is_rejected() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (pa, pb) = (34721, 34722);
                let (ca, cma) = peer(pb);
                let (cb, cmb) = peer(pa);
                let fa = crypto::certificate_fingerprint(&ca);
                // a trusts nobody, b trusts a
                let auth_a = Arc::new(RwLock::new(HashMap::new()));
                let auth_b = Arc::new(RwLock::new(HashMap::from([(fa, "a".into())])));
                let (la, _ra) = start(pa, ca, auth_a, cma).await.unwrap();
                let (lb, _rb) = start(pb, cb, auth_b, cmb).await.unwrap();
                tokio::time::sleep(DIAL_INTERVAL * 2).await;
                assert!(la.senders().is_empty());
                assert!(lb.senders().is_empty());
            })
            .await;
    }
}
