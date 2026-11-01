use lan_mouse_proto::{
    CLIPBOARD_CHUNK_DATA_LEN, ClipboardChunk, ClipboardChunkKind, MAX_CLIPBOARD_TOTAL_LEN,
};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// Compute 32-bit FNV-1a checksum for clipboard change detection and chunk integrity.
pub fn fnv1a_32(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Split a UTF-8 clipboard string into DTLS-safe [`ClipboardChunk`] packets
/// following Deskflow's `StreamChunker::sendClipboard` protocol (`Start` -> `Data`* -> `End`).
pub fn chunk_clipboard_text(transfer_id: u32, text: &str) -> Vec<ClipboardChunk> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_CLIPBOARD_TOTAL_LEN {
        return Vec::new();
    }

    let total_len = bytes.len() as u32;
    let checksum = fnv1a_32(bytes);
    let num_data_chunks = bytes.len().div_ceil(CLIPBOARD_CHUNK_DATA_LEN);
    let mut chunks = Vec::with_capacity(num_data_chunks + 2);

    chunks.push(ClipboardChunk::start(transfer_id, total_len));
    for (idx, slice) in bytes.chunks(CLIPBOARD_CHUNK_DATA_LEN).enumerate() {
        let offset = (idx * CLIPBOARD_CHUNK_DATA_LEN) as u32;
        chunks.push(ClipboardChunk::data(transfer_id, offset, slice));
    }
    chunks.push(ClipboardChunk::end(transfer_id, checksum));
    chunks
}

/// Stateful reassembler and screen-transition clipboard synchronizer.
#[derive(Debug, Default)]
pub struct ClipboardSync {
    next_transfer_id: u32,
    last_synced_hash: Option<u32>,
    active_transfer_id: Option<u32>,
    expected_len: usize,
    buffer: Vec<u8>,
}

impl ClipboardSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed an incoming [`ClipboardChunk`]. Returns `Some(String)` when a full
    /// transfer completes and passes FNV-1a checksum verification.
    pub fn receive_chunk(&mut self, chunk: ClipboardChunk) -> Option<String> {
        match chunk.kind {
            ClipboardChunkKind::Start => {
                let total_len = chunk.offset_or_len as usize;
                if total_len == 0 || total_len > MAX_CLIPBOARD_TOTAL_LEN {
                    log::warn!("rejecting clipboard Start with invalid size: {total_len}");
                    self.reset_transfer();
                    return None;
                }
                self.active_transfer_id = Some(chunk.transfer_id);
                self.expected_len = total_len;
                self.buffer.clear();
                self.buffer.resize(total_len, 0);
                None
            }
            ClipboardChunkKind::Data => {
                if self.active_transfer_id != Some(chunk.transfer_id) {
                    return None;
                }
                let offset = chunk.offset_or_len as usize;
                let payload = chunk.payload();
                let end = offset.saturating_add(payload.len());
                if end > self.expected_len || end > self.buffer.len() {
                    log::warn!(
                        "clipboard Data chunk out of bounds (offset={offset}, len={}, expected={})",
                        payload.len(),
                        self.expected_len
                    );
                    self.reset_transfer();
                    return None;
                }
                self.buffer[offset..end].copy_from_slice(payload);
                None
            }
            ClipboardChunkKind::End => {
                if self.active_transfer_id != Some(chunk.transfer_id) {
                    return None;
                }
                let expected_checksum = chunk.offset_or_len;
                let actual_checksum = fnv1a_32(&self.buffer);
                if actual_checksum != expected_checksum {
                    log::warn!(
                        "clipboard checksum mismatch for transfer {}: expected {expected_checksum:#x}, got {actual_checksum:#x}",
                        chunk.transfer_id
                    );
                    self.reset_transfer();
                    return None;
                }
                let bytes = std::mem::take(&mut self.buffer);
                self.reset_transfer();
                match String::from_utf8(bytes) {
                    Ok(text) => {
                        self.last_synced_hash = Some(actual_checksum);
                        Some(text)
                    }
                    Err(e) => {
                        log::warn!("received non-UTF8 clipboard payload: {e}");
                        None
                    }
                }
            }
        }
    }

    fn reset_transfer(&mut self) {
        self.active_transfer_id = None;
        self.expected_len = 0;
        self.buffer.clear();
    }

    /// Check if the local OS clipboard changed since the last synchronization and,
    /// if so, return the Deskflow-style chunk sequence to send on screen transition.
    pub async fn prepare_outgoing_chunks(&mut self) -> Vec<ClipboardChunk> {
        let Some(text) = read_os_clipboard().await else {
            return Vec::new();
        };
        if text.is_empty() {
            return Vec::new();
        }
        let hash = fnv1a_32(text.as_bytes());
        if self.last_synced_hash == Some(hash) {
            return Vec::new();
        }
        self.last_synced_hash = Some(hash);
        self.next_transfer_id = self.next_transfer_id.wrapping_add(1);
        let id = self.next_transfer_id;
        log::info!(
            "syncing clipboard on screen transition (id={id}, bytes={})",
            text.len()
        );
        chunk_clipboard_text(id, &text)
    }

    /// Process an incoming chunk and write to the local OS clipboard when complete.
    pub async fn handle_incoming_chunk(&mut self, chunk: ClipboardChunk) {
        if let Some(text) = self.receive_chunk(chunk) {
            log::info!("applying incoming clipboard ({} bytes)", text.len());
            if let Err(e) = write_os_clipboard(&text).await {
                log::warn!("failed to write incoming clipboard: {e}");
            }
        }
    }
}

/// Read UTF-8 text from the native OS clipboard without blocking the Tokio loop.
pub async fn read_os_clipboard() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = Command::new("/usr/bin/pbpaste")
            .env("LANG", "en_US.UTF-8")
            .env("LC_CTYPE", "UTF-8")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok()
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(out) = Command::new("wl-paste")
            .arg("--no-newline")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await
        {
            if out.status.success() {
                if let Ok(s) = String::from_utf8(out.stdout) {
                    return Some(s);
                }
            }
        }
        let out = Command::new("xclip")
            .args(["-selection", "clipboard", "-o"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok()
    }

    #[cfg(windows)]
    {
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", "Get-Clipboard -Raw"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok()
    }
}

/// Write UTF-8 text to the native OS clipboard without blocking the Tokio loop.
pub async fn write_os_clipboard(text: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut child = Command::new("/usr/bin/pbcopy")
            .env("LANG", "en_US.UTF-8")
            .env("LC_CTYPE", "UTF-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes()).await?;
        }
        let _ = child.wait().await?;
        Ok(())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(mut child) = Command::new("wl-copy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(text.as_bytes()).await?;
            }
            if child.wait().await.map(|s| s.success()).unwrap_or(false) {
                return Ok(());
            }
        }
        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard", "-i"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes()).await?;
        }
        let _ = child.wait().await?;
        Ok(())
    }

    #[cfg(windows)]
    {
        let mut child = Command::new("powershell")
            .args(["-NoProfile", "-Command", "$input | Set-Clipboard"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes()).await?;
        }
        let _ = child.wait().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_and_reassemble_multilingual_large_payload() {
        let mut sync = ClipboardSync::new();
        let original = "שלום שמעון אזקיאל! Deskflow StreamChunker in Lan Mouse 🚀\n".repeat(150);
        assert!(original.len() > 6000);

        let chunks = chunk_clipboard_text(7, &original);
        assert!(chunks.len() > 12);

        let mut completed = None;
        for chunk in chunks {
            if let Some(res) = sync.receive_chunk(chunk) {
                completed = Some(res);
            }
        }
        assert_eq!(completed.as_deref(), Some(original.as_str()));
    }

    #[test]
    fn corrupted_chunk_is_rejected() {
        let mut sync = ClipboardSync::new();
        let mut chunks = chunk_clipboard_text(9, "Deskflow clipboard integrity check");
        // Corrupt a byte in the Data chunk
        chunks[1].data[0] ^= 0xff;

        for chunk in chunks {
            assert!(sync.receive_chunk(chunk).is_none());
        }
    }

    #[tokio::test]
    async fn e2e_udp_and_os_clipboard_sync_hebrew_english() {
        use lan_mouse_proto::{MAX_EVENT_SIZE, ProtoEvent};
        use tokio::net::UdpSocket;

        // 1. Preserve original OS clipboard so the test is non-destructive
        let original_os_clip = read_os_clipboard().await;

        let test_payload = "בדיקת קופי-פייסט חיה מ-lan-mouse: שלום שמעון אזקיאל! 100% E2E Verified\nLine 2: StreamChunker over UDP/DTLS";
        write_os_clipboard(test_payload)
            .await
            .expect("write_os_clipboard failed");

        let mut host_sync = ClipboardSync::new();
        let mut client_sync = ClipboardSync::new();

        // 2. Host reads real OS clipboard and chunks it for screen transition
        let outgoing_chunks = host_sync.prepare_outgoing_chunks().await;
        assert!(outgoing_chunks.len() >= 3);

        // 3. Overwrite OS clipboard to simulate arriving on a remote machine with different clipboard
        write_os_clipboard("__REMOTE_STALE_CLIPBOARD__")
            .await
            .expect("failed to set stale clipboard");
        assert_eq!(
            read_os_clipboard().await.as_deref(),
            Some("__REMOTE_STALE_CLIPBOARD__")
        );

        // 4. Transmit ProtoEvent::Clipboard frames over real loopback UDP sockets
        let tx_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let rx_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let rx_addr = rx_sock.local_addr().unwrap();

        for chunk in outgoing_chunks {
            let wire_event = ProtoEvent::Clipboard(chunk);
            let (packet, len): ([u8; MAX_EVENT_SIZE], usize) = wire_event.into();
            tx_sock.send_to(&packet[..len], rx_addr).await.unwrap();

            let mut recv_buf = [0u8; MAX_EVENT_SIZE];
            let (amt, _) = rx_sock.recv_from(&mut recv_buf).await.unwrap();
            assert_eq!(amt, len);
            let decoded = ProtoEvent::try_from(recv_buf).expect("valid ProtoEvent on wire");
            match decoded {
                ProtoEvent::Clipboard(rx_chunk) => {
                    client_sync.handle_incoming_chunk(rx_chunk).await;
                }
                other => panic!("unexpected event on wire: {other:?}"),
            }
        }

        // 5. Verify real OS clipboard was updated by client_sync upon receiving the End chunk
        let synced_os_clip = read_os_clipboard()
            .await
            .expect("OS clipboard should be readable after sync");
        assert_eq!(synced_os_clip, test_payload);

        // 6. Verify echo-loop prevention: immediate transition back does NOT re-send identical clipboard
        assert!(client_sync.prepare_outgoing_chunks().await.is_empty());

        // 7. Restore original OS clipboard
        if let Some(orig) = original_os_clip {
            let _ = write_os_clipboard(&orig).await;
        }
    }
}
