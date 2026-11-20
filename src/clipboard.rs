//! Clipboard transfer between lan-mouse peers.
//!
//! Lan-mouse's protocol is UDP-only and carries no clipboard payload, so
//! clipboard sync piggybacks on the existing `enter_hook` mechanism: when the
//! cursor enters a client, we push the local clipboard to that peer over an
//! SSH connection (encrypted, authenticated, reliable).
//!
//! This module provides the default hook commands for text-only clipboard
//! transfer between the OSes lan-mouse supports.

use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

/// Upper bound for clipboard payloads accepted from stdin (1 MiB).
const MAX_CLIPBOARD_SIZE: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("clipboard read failed: {0}")]
    Read(String),
    #[error("clipboard write failed: {0}")]
    Write(String),
    #[error("ssh transport failed: {0}")]
    Transport(String),
}

/// Reads the local (text) clipboard.
/// wayland: wl-paste, x11: xclip, macOS: pbpaste, windows: clip.exe
#[cfg(target_os = "linux")]
async fn read_local_clipboard() -> Result<String, ClipboardError> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        read_from(Command::new("wl-paste").arg("--no-newline"))
            .await
            .map_err(ClipboardError::Read)
    } else {
        read_from(Command::new("xclip").args(["-o", "-selection", "clipboard"]))
            .await
            .map_err(ClipboardError::Read)
    }
}

#[cfg(target_os = "macos")]
async fn read_local_clipboard() -> Result<String, ClipboardError> {
    read_from(&mut Command::new("pbpaste"))
        .await
        .map_err(ClipboardError::Read)
}

#[cfg(target_os = "windows")]
async fn read_local_clipboard() -> Result<String, ClipboardError> {
    read_from(&mut Command::new("powershell").args(["-noprofile", "-command", "Get-Clipboard"]))
        .await
        .map_err(ClipboardError::Read)
}

/// Writes to the local (text) clipboard.
#[cfg(target_os = "linux")]
async fn write_local_clipboard(content: &str) -> Result<(), ClipboardError> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        write_to(&mut Command::new("wl-copy"), content)
            .await
            .map_err(ClipboardError::Write)
    } else {
        write_to(
            Command::new("xclip").args(["-selection", "clipboard"]),
            content,
        )
        .await
        .map_err(ClipboardError::Write)
    }
}

#[cfg(target_os = "macos")]
async fn write_local_clipboard(content: &str) -> Result<(), ClipboardError> {
    write_to(&mut Command::new("pbcopy"), content)
        .await
        .map_err(ClipboardError::Write)
}

#[cfg(target_os = "windows")]
async fn write_local_clipboard(content: &str) -> Result<(), ClipboardError> {
    write_to(&mut Command::new("clip"), content)
        .await
        .map_err(ClipboardError::Write)
}

/// Push the local clipboard to a remote lan-mouse peer over SSH.
///
/// The remote runs `lan-mouse receive-clipboard` (patched peers) or falls
/// back to pbcopy/wl-copy/clip (unpatched peers).
pub async fn push_clipboard_to(host: &str) -> Result<(), ClipboardError> {
    let content = read_local_clipboard().await?;
    if content.is_empty() {
        return Ok(());
    }
    // Remote receiver: prefer a patched lan-mouse peer, then fall back to
    // the OS clipboard tool so unpatched peers (e.g. macOS running upstream
    // Lan Mouse) work out of the box.
    const REMOTE_CMD: &str = "sh -c 'command -v lan-mouse >/dev/null 2>&1 && exec lan-mouse receive-clipboard; command -v pbcopy >/dev/null 2>&1 && exec pbcopy; command -v wl-copy >/dev/null 2>&1 && exec wl-copy; exec clip'";
    let mut ssh = Command::new("ssh");
    ssh.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=5",
        host,
        REMOTE_CMD,
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    write_to(&mut ssh, &content)
        .await
        .map_err(ClipboardError::Transport)
}

/// Entry point for the remote side: read payload from stdin into the local
/// clipboard. Invoked via SSH from a patched sender.
pub async fn receive_clipboard() -> Result<(), ClipboardError> {
    let mut content = String::new();
    tokio::io::stdin()
        .take(MAX_CLIPBOARD_SIZE)
        .read_to_string(&mut content)
        .await
        .map_err(|e| ClipboardError::Read(e.to_string()))?;
    if content.is_empty() {
        return Ok(());
    }
    write_local_clipboard(&content).await
}

/// Pull the clipboard from a remote peer over SSH into the local clipboard.
/// Used when the cursor returns from a peer (Leave).
const REMOTE_PULL_CMD: &str = "sh -c 'command -v pbpaste >/dev/null 2>&1 && exec pbpaste; command -v wl-paste >/dev/null 2>&1 && exec wl-paste --no-newline; exec powershell -noprofile -command Get-Clipboard'";

pub async fn pull_clipboard_from(host: &str) -> Result<(), ClipboardError> {
    let mut ssh = Command::new("ssh");
    ssh.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=5",
        host,
        REMOTE_PULL_CMD,
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let content = read_from(&mut ssh)
        .await
        .map_err(ClipboardError::Transport)?;
    let content = content.trim_end_matches('\n');
    if content.is_empty() {
        return Ok(());
    }
    write_local_clipboard(content).await
}

async fn read_from(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.output().await.map_err(|e| {
        format!(
            "failed to spawn {}: {e}",
            cmd.as_std().get_program().to_string_lossy()
        )
    })?;
    if !out.status.success() {
        return Err(format!(
            "exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn write_to(cmd: &mut Command, content: &str) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    // the child consumes `content` via stdin — always pipe it, otherwise
    // the child inherits our (already drained) stdin and reads EOF
    cmd.stdin(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "failed to spawn {}: {e}",
            cmd.as_std().get_program().to_string_lossy()
        )
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(content.as_bytes())
            .await
            .map_err(|e| format!("write failed: {e}"))?;
    }
    let status = child
        .wait()
        .await
        .map_err(|e| format!("wait failed: {e}"))?;
    if !status.success() {
        return Err(format!("exit {status}"));
    }
    Ok(())
}
