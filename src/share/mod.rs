//! Clipboard and file sharing between devices.
//!
//! Whenever the local clipboard changes, its contents are sent to every
//! linked device, which puts them on its own clipboard, so paste works right
//! after moving to another screen. Files copied in the file manager are
//! streamed to the other devices' download folder; once complete, the
//! received files are put on that device's clipboard, ready to paste.

mod clipboard;
mod files;
mod link;
mod proto;

use std::{
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use tokio::{sync::mpsc, task::spawn_local};
use tokio_util::sync::CancellationToken;
use webrtc_dtls::crypto::Certificate;

use crate::client::ClientManager;
use clipboard::ClipData;
use link::{Fingerprint, LinkEvent, Links, short};
use proto::{ClipKind, ClipPart, Frame, Header};

/// clipboard tools often write several times per copy; wait for quiet
const DEBOUNCE: Duration = Duration::from_millis(150);
/// after we write the clipboard, ignore change notifications this long
const SUPPRESS: Duration = Duration::from_millis(1200);
/// largest clipboard contents (text + images) we send
const MAX_CLIP: u64 = 64 * 1024 * 1024;

pub(crate) struct ShareConfig {
    pub port: u16,
    pub clipboard: bool,
    pub files: bool,
    pub file_auto_limit: u64,
    pub download_dir: PathBuf,
}

impl ShareConfig {
    pub(crate) fn from_config(config: &crate::config::Config) -> Self {
        Self {
            port: config.port(),
            clipboard: config.clipboard_enabled(),
            files: config.file_transfer_enabled(),
            file_auto_limit: config.file_auto_limit(),
            download_dir: config
                .download_dir()
                .unwrap_or_else(files::default_download_dir),
        }
    }
}

pub(crate) fn spawn(
    config: ShareConfig,
    cert: Certificate,
    authorized: Arc<RwLock<HashMap<String, String>>>,
    client_manager: ClientManager,
) {
    if !config.clipboard && !config.files {
        return;
    }
    spawn_local(async move {
        let (links, events) = match link::start(config.port, cert, authorized, client_manager).await
        {
            Ok(l) => l,
            Err(e) => {
                log::warn!("clipboard and file sharing unavailable: {e}");
                return;
            }
        };
        let (clip_tx, clip_rx) = mpsc::unbounded_channel();
        clipboard::watch(clip_tx);
        let mut share = Share {
            config,
            links,
            generation: 0,
            last_seen: None,
            last_applied: None,
            suppress_until: Instant::now(),
            next_batch: 0,
            outgoing: None,
            incoming: HashMap::new(),
        };
        share.run(events, clip_rx).await;
    });
}

struct Share {
    config: ShareConfig,
    links: Links,
    /// counts genuine local clipboard changes
    generation: u64,
    /// digest of the last local contents handled
    last_seen: Option<[u8; 32]>,
    /// digest of the last contents received and written by us
    last_applied: Option<[u8; 32]>,
    suppress_until: Instant,
    next_batch: u64,
    outgoing: Option<CancellationToken>,
    incoming: HashMap<(Fingerprint, u64), files::Incoming>,
}

impl Share {
    async fn run(
        &mut self,
        mut events: mpsc::Receiver<LinkEvent>,
        mut clip_rx: mpsc::UnboundedReceiver<()>,
    ) {
        loop {
            tokio::select! {
                n = clip_rx.recv() => {
                    if n.is_none() {
                        log::warn!("clipboard watcher stopped");
                        std::future::pending::<()>().await;
                    }
                    tokio::time::sleep(DEBOUNCE).await;
                    while clip_rx.try_recv().is_ok() {}
                    self.local_change().await;
                }
                ev = events.recv() => match ev {
                    Some(ev) => self.link_event(ev).await,
                    None => return,
                },
            }
        }
    }

    async fn local_change(&mut self) {
        if Instant::now() < self.suppress_until {
            return;
        }
        let Some(data) = tokio::task::spawn_blocking(clipboard::read)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        let digest = data.digest();
        if Some(digest) == self.last_applied || Some(digest) == self.last_seen {
            return;
        }
        self.last_seen = Some(digest);
        self.generation += 1;
        if let Some(cancel) = self.outgoing.take() {
            cancel.cancel();
        }
        log::debug!("local clipboard changed: {}", data.describe());
        match data {
            ClipData::Parts(parts) => {
                if !self.config.clipboard {
                    return;
                }
                let size: u64 = parts.iter().map(|(_, d)| d.len() as u64).sum();
                if size > MAX_CLIP {
                    log::info!("clipboard too large to share ({size} bytes)");
                    return;
                }
                for (fp, link) in self.links.senders() {
                    log::debug!("sending clipboard to {}", short(&fp));
                    link.send(clip_frame(&parts));
                }
            }
            ClipData::Files(paths) => {
                if self.config.files {
                    self.send_files(paths).await;
                }
            }
        }
    }

    async fn send_files(&mut self, paths: Vec<PathBuf>) {
        let out = match tokio::task::spawn_blocking(move || files::collect(&paths)).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                log::warn!("cannot read copied files: {e}");
                return;
            }
            Err(_) => return,
        };
        if out.total > self.config.file_auto_limit {
            log::info!(
                "copied files total {} MiB, above the automatic limit of {} MiB; not sending",
                out.total >> 20,
                self.config.file_auto_limit >> 20
            );
            return;
        }
        let links = self.links.senders();
        if links.is_empty() {
            return;
        }
        self.next_batch += 1;
        let batch = self.next_batch;
        let cancel = CancellationToken::new();
        self.outgoing = Some(cancel.clone());
        let out = Rc::new(out);
        for (fp, link) in links {
            log::info!(
                "sending {} item(s), {} bytes, to {}",
                out.entries.len(),
                out.total,
                short(&fp)
            );
            link.send(Frame::new(Header::FilesBegin {
                batch,
                entries: out.entries.clone(),
                total: out.total,
            }));
            let out = out.clone();
            let cancel = cancel.clone();
            spawn_local(async move {
                let started = Instant::now();
                tokio::select! {
                    ok = files::send(&link, batch, &out) => {
                        if ok {
                            let secs = started.elapsed().as_secs_f64().max(0.001);
                            log::info!(
                                "sent files to {} ({:.1} MiB/s)",
                                short(&fp),
                                out.total as f64 / secs / 1048576.0
                            );
                        } else {
                            link.send(Frame::new(Header::FilesCancel { batch }));
                        }
                    }
                    _ = cancel.cancelled() => {
                        log::info!("file transfer to {} cancelled: clipboard changed", short(&fp));
                        link.send(Frame::new(Header::FilesCancel { batch }));
                    }
                }
            });
        }
    }

    async fn link_event(&mut self, ev: LinkEvent) {
        match ev {
            // Contents copied while unlinked are not sent on link up: both
            // sides would send theirs and end up swapping clipboards.
            LinkEvent::Up(fp) => log::debug!("linked with {}", short(&fp)),
            LinkEvent::Down(fp) => {
                let keys: Vec<_> = self
                    .incoming
                    .keys()
                    .filter(|(f, _)| *f == fp)
                    .cloned()
                    .collect();
                for k in keys {
                    if let Some(inc) = self.incoming.remove(&k) {
                        log::warn!("file transfer from {} interrupted", short(&fp));
                        inc.discard();
                    }
                }
            }
            LinkEvent::Frame(fp, frame) => self.frame(fp, frame).await,
        }
    }

    async fn frame(&mut self, fp: Fingerprint, frame: Frame) {
        match frame.header {
            Header::Ping => {}
            Header::Clip { parts } => {
                if !self.config.clipboard {
                    return;
                }
                let Some(parts) = split_parts(&parts, frame.body) else {
                    log::warn!("malformed clipboard from {}", short(&fp));
                    return;
                };
                let data = ClipData::Parts(parts);
                log::debug!("clipboard from {}: {}", short(&fp), data.describe());
                self.apply(data).await;
            }
            Header::FilesBegin {
                batch,
                entries,
                total,
            } => {
                if !self.config.files {
                    return;
                }
                if total > self.config.file_auto_limit {
                    log::info!(
                        "ignoring {total} bytes of files from {}: above limit",
                        short(&fp)
                    );
                    return;
                }
                match files::Incoming::begin(
                    &self.config.download_dir,
                    entries,
                    total,
                    self.generation,
                ) {
                    Ok(inc) => {
                        log::info!("receiving {total} bytes of files from {}", short(&fp));
                        self.incoming.insert((fp, batch), inc);
                    }
                    Err(e) => log::warn!("rejecting files from {}: {e}", short(&fp)),
                }
            }
            Header::FileData { batch, index } => {
                let key = (fp, batch);
                let Some(inc) = self.incoming.get_mut(&key) else {
                    return;
                };
                if let Err(e) = inc.data(index, &frame.body).await {
                    log::warn!("file transfer from {} failed: {e}", short(&key.0));
                    if let Some(inc) = self.incoming.remove(&key) {
                        inc.discard();
                    }
                }
            }
            Header::FilesEnd { batch } => {
                let Some(mut inc) = self.incoming.remove(&(fp.clone(), batch)) else {
                    return;
                };
                if let Err(e) = inc.end().await {
                    log::warn!("file transfer from {} failed: {e}", short(&fp));
                    inc.discard();
                    return;
                }
                log::info!(
                    "received {} bytes of files from {} into {:?}",
                    inc.total,
                    short(&fp),
                    self.config.download_dir
                );
                if inc.clip_generation == self.generation {
                    self.apply(ClipData::Files(inc.roots.clone())).await;
                } else {
                    log::info!("clipboard changed meanwhile; files kept in the download folder");
                }
            }
            Header::FilesCancel { batch } => {
                if let Some(inc) = self.incoming.remove(&(fp.clone(), batch)) {
                    log::info!("file transfer from {} cancelled by sender", short(&fp));
                    inc.discard();
                }
            }
        }
    }

    /// Put received contents on the local clipboard without echoing them back.
    async fn apply(&mut self, data: ClipData) {
        let digest = data.digest();
        self.last_applied = Some(digest);
        self.last_seen = Some(digest);
        self.suppress_until = Instant::now() + SUPPRESS;
        let result = tokio::task::spawn_blocking(move || clipboard::write(&data)).await;
        self.suppress_until = Instant::now() + SUPPRESS;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => log::warn!("could not set clipboard: {e}"),
            Err(e) => log::warn!("could not set clipboard: {e}"),
        }
    }
}

fn clip_frame(parts: &[(ClipKind, Vec<u8>)]) -> Frame {
    let header = Header::Clip {
        parts: parts
            .iter()
            .map(|(kind, d)| ClipPart {
                kind: *kind,
                len: d.len() as u64,
            })
            .collect(),
    };
    let body = parts.iter().flat_map(|(_, d)| d.iter().copied()).collect();
    Frame::with_body(header, body)
}

fn split_parts(parts: &[ClipPart], body: Vec<u8>) -> Option<Vec<(ClipKind, Vec<u8>)>> {
    let total: u64 = parts.iter().map(|p| p.len).sum();
    if total != body.len() as u64 {
        return None;
    }
    let mut out = Vec::new();
    let mut offset = 0usize;
    for p in parts {
        let end = offset + p.len as usize;
        out.push((p.kind, body[offset..end].to_vec()));
        offset = end;
    }
    Some(out)
}
