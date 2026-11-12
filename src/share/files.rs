//! Sending and receiving files that were copied to the clipboard.

use std::{
    collections::HashMap,
    fs, io,
    path::{Component, Path, PathBuf},
};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{
    link::LinkSender,
    proto::{FileEntry, Frame, Header, MAX_BODY},
};

const CHUNK: usize = 256 * 1024;

/// Files to send: wire entries plus where each one is on disk.
pub(crate) struct Outgoing {
    pub entries: Vec<FileEntry>,
    sources: Vec<PathBuf>,
    pub total: u64,
}

/// Walk the copied paths (files and folders). Symbolic links are skipped.
pub(crate) fn collect(paths: &[PathBuf]) -> io::Result<Outgoing> {
    let mut out = Outgoing {
        entries: Vec::new(),
        sources: Vec::new(),
        total: 0,
    };
    for path in paths {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        walk(path, name, &mut out)?;
    }
    Ok(out)
}

fn walk(path: &Path, rel: String, out: &mut Outgoing) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        out.entries.push(FileEntry {
            path: rel.clone(),
            size: 0,
            dir: true,
        });
        out.sources.push(path.to_owned());
        let mut children: Vec<_> = fs::read_dir(path)?.filter_map(|e| e.ok()).collect();
        children.sort_by_key(|e| e.file_name());
        for child in children {
            let name = child.file_name().to_string_lossy().into_owned();
            walk(&child.path(), format!("{rel}/{name}"), out)?;
        }
    } else if meta.is_file() {
        out.total += meta.len();
        out.entries.push(FileEntry {
            path: rel,
            size: meta.len(),
            dir: false,
        });
        out.sources.push(path.to_owned());
    }
    Ok(())
}

/// Stream all files of a batch over a link. Returns false if cancelled or
/// the link went away.
pub(crate) async fn send(link: &LinkSender, batch: u64, out: &Outgoing) -> bool {
    let mut buf = vec![0u8; CHUNK];
    for (index, (entry, src)) in out.entries.iter().zip(&out.sources).enumerate() {
        if entry.dir {
            continue;
        }
        let mut file = match tokio::fs::File::open(src).await {
            Ok(f) => f,
            Err(e) => {
                log::warn!("cannot read {src:?}: {e}");
                return false;
            }
        };
        let mut remaining = entry.size;
        while remaining > 0 {
            let want = (remaining as usize).min(CHUNK);
            if let Err(e) = file.read_exact(&mut buf[..want]).await {
                log::warn!("reading {src:?}: {e}");
                return false;
            }
            remaining -= want as u64;
            let frame = Frame::with_body(
                Header::FileData {
                    batch,
                    index: index as u32,
                },
                buf[..want].to_vec(),
            );
            if link.send_bulk(frame).await.is_err() {
                return false;
            }
        }
    }
    link.send_bulk(Frame::new(Header::FilesEnd { batch }))
        .await
        .is_ok()
}

/// A '/'-separated relative path from a peer, made safe to join onto our
/// download folder: no absolute paths, drive letters, `..` or empty parts.
fn safe_relative(path: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains(['\\', ':', '\0']) {
            return None;
        }
        out.push(part);
    }
    // belt and braces: everything must be a normal component
    if out.components().all(|c| matches!(c, Component::Normal(_))) {
        Some(out)
    } else {
        None
    }
}

/// `dir/name`, or `dir/name (2).ext` etc. if that exists already.
fn unique_name(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let p = Path::new(name);
    let (stem, ext) = match (p.file_stem(), p.extension()) {
        (Some(s), Some(e)) if !name.starts_with('.') => (
            s.to_string_lossy().into_owned(),
            format!(".{}", e.to_string_lossy()),
        ),
        _ => (name.to_owned(), String::new()),
    };
    (2..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|c| !c.exists())
        .expect("some name is free")
}

/// A batch being received.
pub(crate) struct Incoming {
    entries: Vec<FileEntry>,
    /// destination of every entry
    targets: Vec<PathBuf>,
    /// top-level items, put on the clipboard when done
    pub roots: Vec<PathBuf>,
    current: Option<(u32, tokio::fs::File, u64)>,
    pub total: u64,
    /// local clipboard generation when the batch started
    pub clip_generation: u64,
}

impl Incoming {
    pub(crate) fn begin(
        download_dir: &Path,
        entries: Vec<FileEntry>,
        total: u64,
        clip_generation: u64,
    ) -> io::Result<Self> {
        let invalid = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_owned());
        if entries.iter().map(|e| e.size).sum::<u64>() != total {
            return Err(invalid("sizes don't add up"));
        }
        fs::create_dir_all(download_dir)?;
        // map each top-level name to a free name in the download folder
        let mut renamed: HashMap<String, PathBuf> = HashMap::new();
        let mut roots = Vec::new();
        let mut targets = Vec::new();
        for entry in &entries {
            let rel = safe_relative(&entry.path).ok_or_else(|| invalid("unsafe path"))?;
            let mut parts = rel.components();
            let top = parts
                .next()
                .ok_or_else(|| invalid("empty path"))?
                .as_os_str()
                .to_string_lossy()
                .into_owned();
            let root = renamed
                .entry(top.clone())
                .or_insert_with(|| {
                    let r = unique_name(download_dir, &top);
                    roots.push(r.clone());
                    r
                })
                .clone();
            let rest = parts.as_path();
            targets.push(if rest.as_os_str().is_empty() {
                root
            } else {
                root.join(rest)
            });
        }
        Ok(Self {
            entries,
            targets,
            roots,
            current: None,
            total,
            clip_generation,
        })
    }

    pub(crate) async fn data(&mut self, index: u32, data: &[u8]) -> io::Result<()> {
        let invalid = |msg: &str| io::Error::new(io::ErrorKind::InvalidData, msg.to_owned());
        let i = index as usize;
        let entry = self.entries.get(i).ok_or_else(|| invalid("bad index"))?;
        let (is_dir, size) = (entry.dir, entry.size);
        if is_dir || data.len() as u64 > MAX_BODY {
            return Err(invalid("unexpected data"));
        }
        if self.current.as_ref().map(|c| c.0) != Some(index) {
            self.finish_current().await?;
            let target = &self.targets[i];
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let file = tokio::fs::File::create(target).await?;
            self.current = Some((index, file, 0));
        }
        let (_, file, written) = self.current.as_mut().expect("current file");
        *written += data.len() as u64;
        if *written > size {
            return Err(invalid("file larger than announced"));
        }
        file.write_all(data).await
    }

    async fn finish_current(&mut self) -> io::Result<()> {
        if let Some((index, mut file, written)) = self.current.take() {
            file.flush().await?;
            if written != self.entries[index as usize].size {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "file truncated"));
            }
        }
        Ok(())
    }

    /// Complete the batch: create empty files and folders that had no data.
    pub(crate) async fn end(&mut self) -> io::Result<()> {
        self.finish_current().await?;
        for (entry, target) in self.entries.iter().zip(&self.targets) {
            if entry.dir {
                tokio::fs::create_dir_all(target).await?;
            } else if !target.exists() {
                if entry.size != 0 {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "file missing"));
                }
                if let Some(parent) = target.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::File::create(target).await?;
            }
        }
        Ok(())
    }

    /// Remove whatever was written so far.
    pub(crate) fn discard(mut self) {
        self.current = None;
        for root in &self.roots {
            let _ = if root.is_dir() {
                fs::remove_dir_all(root)
            } else {
                fs::remove_file(root)
            };
        }
    }
}

pub(crate) fn default_download_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Downloads").join("Lan Mouse")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_paths() {
        for p in [
            "../x",
            "a/../../x",
            "/etc/passwd",
            "C:/x",
            "a\\..\\x",
            "",
            "a//b",
            "./a",
        ] {
            assert!(safe_relative(p).is_none(), "{p}");
        }
        assert_eq!(
            safe_relative("a/b c/d.txt"),
            Some(PathBuf::from("a").join("b c").join("d.txt"))
        );
    }

    #[tokio::test]
    async fn roundtrip_folder() {
        let base = std::env::temp_dir().join(format!("lm-files-{}", std::process::id()));
        let src = base.join("src");
        let dst = base.join("dst");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(src.join("folder/sub")).unwrap();
        fs::write(src.join("folder/a.txt"), b"hello").unwrap();
        fs::write(src.join("folder/sub/empty"), b"").unwrap();
        let big: Vec<u8> = (0..(CHUNK * 2 + 17)).map(|i| i as u8).collect();
        fs::write(src.join("big.bin"), &big).unwrap();

        let out = collect(&[src.join("folder"), src.join("big.bin")]).unwrap();
        assert_eq!(out.total, 5 + big.len() as u64);

        let mut inc = Incoming::begin(&dst, out.entries.clone(), out.total, 0).unwrap();
        let mut buf = vec![0u8; CHUNK];
        for (i, (e, s)) in out.entries.iter().zip(&out.sources).enumerate() {
            if e.dir {
                continue;
            }
            let mut f = fs::File::open(s).unwrap();
            let mut left = e.size as usize;
            while left > 0 {
                let n = left.min(CHUNK);
                io::Read::read_exact(&mut f, &mut buf[..n]).unwrap();
                inc.data(i as u32, &buf[..n]).await.unwrap();
                left -= n;
            }
        }
        inc.end().await.unwrap();
        assert_eq!(fs::read(dst.join("folder/a.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(dst.join("folder/sub/empty")).unwrap(), b"");
        assert_eq!(fs::read(dst.join("big.bin")).unwrap(), big);

        // a second copy gets new names instead of overwriting
        let inc2 = Incoming::begin(&dst, out.entries.clone(), out.total, 0).unwrap();
        assert!(inc2.roots.contains(&dst.join("folder (2)")));
        assert!(inc2.roots.contains(&dst.join("big (2).bin")));
        let _ = fs::remove_dir_all(&base);
    }
}
