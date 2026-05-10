//! TCC.db mtime watcher → fresh-subprocess probe → daemon exit.
//!
//! Detects "user removed the app from System Settings → Privacy &
//! Security → Accessibility" — the case where `AXIsProcessTrusted()`
//! in already-running processes keeps reporting cached-true for an
//! unbounded window, even though the OS has effectively revoked
//! permission and a fresh process would see false.
//!
//! Strategy: poll the TCC database file's mtime every 2 seconds. The
//! stat is essentially free (kernel-cached inode metadata, ~µs per
//! call). When the mtime ticks, spawn a fresh subprocess of our own
//! binary that runs `lan-mouse ax-probe` — a fresh process bypasses
//! the parent's cached trust and consults the current TCC state. If
//! the probe exits non-zero (AX revoked), we exit the daemon with
//! `process::exit(0)`. The GUI's IPC-drop watcher then propagates
//! the exit (see `lan_mouse_gtk::lib::receiver.recv` Err branch).
//!
//! Cost is negligible: one stat per 2s, plus one subprocess spawn
//! per actual TCC change (rare — only fires when the user opens
//! System Settings and modifies the list).

use std::env;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::time::{Duration, SystemTime};

const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// System-level TCC database — holds Accessibility, Input Monitoring,
/// Screen Recording, etc. for application bundles. This is the file
/// that ticks when the user adds/removes/toggles entries in System
/// Settings → Privacy & Security → Accessibility. The per-user
/// `~/Library/Application Support/com.apple.TCC/TCC.db` does NOT
/// track Accessibility — confirmed empirically; an earlier version
/// of this watcher polled both and only the system path moved on
/// the AX revoke path that this module exists to fix.
const TCC_DB_PATH: &str = "/Library/Application Support/com.apple.TCC/TCC.db";

fn read_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn probe_ax_in_fresh_subprocess() -> Option<bool> {
    // Spawn a fresh copy of our binary with `ax-probe`. A fresh
    // process consults the current TCC state without inheriting the
    // parent's cached trust. Exit 0 = granted, 1 = revoked.
    let exe = env::current_exe().ok()?;
    let status = Command::new(exe).arg("ax-probe").status().ok()?;
    Some(status.success())
}

/// Spawn the watcher on the current tokio runtime. Must be called
/// from inside a tokio runtime; runs forever until the daemon exits.
pub fn spawn() {
    tokio::spawn(async move {
        let path = PathBuf::from(TCC_DB_PATH);
        let mut last_mtime = read_mtime(&path);
        log::info!(
            "tcc_watch: watching {} (initial mtime: {:?})",
            path.display(),
            last_mtime
        );

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let current_mtime = read_mtime(&path);
            if current_mtime == last_mtime {
                continue;
            }
            log::info!(
                "tcc_watch: TCC.db mtime changed ({last_mtime:?} -> {current_mtime:?}) — confirming AX via fresh subprocess"
            );
            last_mtime = current_mtime;
            match probe_ax_in_fresh_subprocess() {
                Some(true) => {
                    log::debug!("tcc_watch: probe confirms AX still granted");
                }
                Some(false) => {
                    log::error!(
                        "tcc_watch: AX revoked (TCC.db changed and fresh probe returned false) — daemon exiting"
                    );
                    process::exit(0);
                }
                None => {
                    log::warn!("tcc_watch: probe subprocess failed to spawn or run");
                }
            }
        }
    });
}
