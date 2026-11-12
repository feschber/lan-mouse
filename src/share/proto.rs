//! Wire format of the data link: every frame is
//! `u32 header length | JSON header | u64 body length | body`, big endian.

use serde::{Deserialize, Serialize};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest header we accept (file lists can be long).
const MAX_HEADER: usize = 16 * 1024 * 1024;
/// Largest body we accept: a clipboard image or one file chunk.
pub(crate) const MAX_BODY: u64 = 128 * 1024 * 1024;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClipKind {
    Text,
    Html,
    Rtf,
    Png,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct ClipPart {
    pub kind: ClipKind,
    pub len: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileEntry {
    /// '/'-separated path relative to the transfer root
    pub path: String,
    pub size: u64,
    pub dir: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "t", rename_all = "snake_case")]
pub(crate) enum Header {
    Ping,
    /// clipboard contents; the body holds the parts back to back
    Clip {
        parts: Vec<ClipPart>,
    },
    /// files copied to the clipboard; `FileData` frames follow in order
    FilesBegin {
        batch: u64,
        entries: Vec<FileEntry>,
        total: u64,
    },
    /// a chunk of entry `index`; the body holds the data
    FileData {
        batch: u64,
        index: u32,
    },
    FilesEnd {
        batch: u64,
    },
    FilesCancel {
        batch: u64,
    },
}

pub(crate) struct Frame {
    pub header: Header,
    pub body: Vec<u8>,
}

impl Frame {
    pub(crate) fn new(header: Header) -> Self {
        Self {
            header,
            body: Vec::new(),
        }
    }
    pub(crate) fn with_body(header: Header, body: Vec<u8>) -> Self {
        Self { header, body }
    }
}

pub(crate) async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) -> io::Result<()> {
    let header = serde_json::to_vec(&frame.header).map_err(io::Error::other)?;
    w.write_u32(header.len() as u32).await?;
    w.write_all(&header).await?;
    w.write_u64(frame.body.len() as u64).await?;
    w.write_all(&frame.body).await?;
    Ok(())
}

pub(crate) async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Frame> {
    let header_len = r.read_u32().await? as usize;
    if header_len > MAX_HEADER {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "header too large",
        ));
    }
    let mut header = vec![0u8; header_len];
    r.read_exact(&mut header).await?;
    let header: Header = serde_json::from_slice(&header)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let body_len = r.read_u64().await?;
    if body_len > MAX_BODY {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "body too large"));
    }
    let mut body = vec![0u8; body_len as usize];
    r.read_exact(&mut body).await?;
    Ok(Frame { header, body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip() {
        let frame = Frame::with_body(
            Header::Clip {
                parts: vec![ClipPart {
                    kind: ClipKind::Text,
                    len: 5,
                }],
            },
            b"hello".to_vec(),
        );
        let mut buf = Vec::new();
        write_frame(&mut buf, &frame).await.unwrap();
        let back = read_frame(&mut buf.as_slice()).await.unwrap();
        assert_eq!(back.body, b"hello");
        assert!(matches!(back.header, Header::Clip { ref parts } if parts.len() == 1));
    }
}
