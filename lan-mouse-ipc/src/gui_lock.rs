//! Cross-platform GUI singleton.
//!
//! Lan Mouse can be launched as: a headless daemon (`lan-mouse daemon`),
//! or a GUI that owns a child daemon (`lan-mouse` with no args). The
//! daemon is already a singleton via its IPC socket. This module
//! provides the *GUI*-side singleton: a separate socket that exists
//! only while a GUI is running, decoupled from daemon state so the
//! headless-then-GUI case still works.
//!
//! Acquire path: bind the socket. On success, this process is the
//! primary GUI; spawn a listener on the bound socket and call
//! `next_show()` to receive show requests from later launches.
//!
//! Signal path: if the bind fails because another GUI is already
//! listening, connect to it, send a single byte, exit cleanly. The
//! primary GUI's `next_show()` returns `Some(())` and the GUI
//! presents its window.
//!
//! The mechanism is identical on all three platforms — Unix sockets
//! on Linux/macOS, a localhost TCP listener on Windows — and works
//! whether the daemon is embedded (`lan-mouse`) or standalone
//! (`lan-mouse daemon`), because no daemon round-trip is involved.

use std::io::{self, Read, Write};

#[cfg(unix)]
use std::{
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
};

#[cfg(windows)]
use std::net::{TcpListener, TcpStream};

use thiserror::Error;

use crate::SocketPathError;

#[cfg(unix)]
const LAN_MOUSE_GUI_SOCKET_NAME: &str = "lan-mouse-gui.sock";

#[cfg(windows)]
const LAN_MOUSE_GUI_TCP: &str = "127.0.0.1:5253";

/// Single-byte message: "show your window".
const SHOW_BYTE: u8 = 0x01;

#[derive(Debug, Error)]
pub enum GuiLockError {
    #[error(transparent)]
    SocketPath(#[from] SocketPathError),
    #[error("io error: `{0}`")]
    Io(#[from] io::Error),
}

#[cfg(all(unix, not(target_os = "macos")))]
fn gui_socket_path() -> Result<PathBuf, SocketPathError> {
    let xdg_runtime_dir =
        std::env::var("XDG_RUNTIME_DIR").map_err(SocketPathError::XdgRuntimeDirNotFound)?;
    Ok(Path::new(&xdg_runtime_dir).join(LAN_MOUSE_GUI_SOCKET_NAME))
}

#[cfg(all(unix, target_os = "macos"))]
fn gui_socket_path() -> Result<PathBuf, SocketPathError> {
    let home = std::env::var("HOME").map_err(SocketPathError::HomeDirNotFound)?;
    Ok(Path::new(&home)
        .join("Library")
        .join("Caches")
        .join(LAN_MOUSE_GUI_SOCKET_NAME))
}

pub struct GuiLock {
    #[cfg(unix)]
    listener: UnixListener,
    #[cfg(unix)]
    socket_path: PathBuf,
    #[cfg(windows)]
    listener: TcpListener,
}

impl GuiLock {
    /// Acquire the GUI singleton, or signal an existing GUI to show
    /// itself.
    ///
    /// Returns `Ok(Some(lock))` when this process is the primary GUI;
    /// keep the lock alive for the GUI's lifetime. Returns `Ok(None)`
    /// when another GUI was already listening — the show byte has
    /// been sent and the caller should exit.
    pub fn acquire_or_signal() -> Result<Option<Self>, GuiLockError> {
        #[cfg(unix)]
        {
            let socket_path = gui_socket_path()?;
            if socket_path.exists() {
                match UnixStream::connect(&socket_path) {
                    Ok(mut stream) => {
                        stream.write_all(&[SHOW_BYTE])?;
                        return Ok(None);
                    }
                    Err(_) => {
                        // Stale socket from a crashed GUI — clear it.
                        let _ = std::fs::remove_file(&socket_path);
                    }
                }
            }
            let listener = UnixListener::bind(&socket_path)?;
            Ok(Some(Self {
                listener,
                socket_path,
            }))
        }
        #[cfg(windows)]
        {
            if let Ok(mut stream) = TcpStream::connect(LAN_MOUSE_GUI_TCP) {
                stream.write_all(&[SHOW_BYTE])?;
                return Ok(None);
            }
            let listener = TcpListener::bind(LAN_MOUSE_GUI_TCP)?;
            Ok(Some(Self { listener }))
        }
    }

    /// Block until a show request arrives. Returns `Some(())` for a
    /// valid show byte, `None` if the listener was closed or a
    /// malformed message was received (caller should keep looping).
    pub fn next_show(&self) -> Option<()> {
        let (mut stream, _) = self.listener.accept().ok()?;
        let mut buf = [0u8; 1];
        match stream.read_exact(&mut buf) {
            Ok(()) if buf[0] == SHOW_BYTE => Some(()),
            _ => Some(()), // Treat any non-fatal read as a poke.
        }
    }
}

#[cfg(unix)]
impl Drop for GuiLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Acquire-twice should return Ok(Some) the first time and Ok(None)
    /// (after sending the show byte) the second time. Drop the lock and
    /// the next acquire should succeed again.
    #[test]
    fn acquire_then_signal_then_re_acquire() {
        // Use a short path under /tmp so the resulting sockaddr_un fits
        // in SUN_LEN (108 bytes on Linux, 104 on macOS) — XDG_RUNTIME_DIR
        // and the macOS HOME/Library/Caches path can both blow past
        // that when nested inside a tempdir.
        let dir = std::path::PathBuf::from(format!("/tmp/lmt{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        #[cfg(target_os = "macos")]
        let original_home = std::env::var("HOME").ok();
        #[cfg(target_os = "macos")]
        unsafe {
            std::env::set_var("HOME", &dir);
            std::fs::create_dir_all(dir.join("Library").join("Caches")).ok();
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        let original = std::env::var("XDG_RUNTIME_DIR").ok();
        #[cfg(all(unix, not(target_os = "macos")))]
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }

        let lock = GuiLock::acquire_or_signal()
            .expect("first acquire ok")
            .expect("first acquire is primary");

        // Spawn a thread to wait for the show signal.
        let handle = std::thread::spawn(move || {
            let got = lock.next_show();
            (got, lock)
        });

        // Tiny sleep so the listener thread is parked in accept().
        std::thread::sleep(Duration::from_millis(50));

        // Second acquire should signal-and-return-None.
        let res = GuiLock::acquire_or_signal().expect("second acquire ok");
        assert!(res.is_none(), "second acquire must signal-and-exit");

        let (got, lock) = handle.join().unwrap();
        assert_eq!(got, Some(()));

        drop(lock);

        // After dropping, acquire should succeed again as primary.
        let _lock2 = GuiLock::acquire_or_signal()
            .expect("third acquire ok")
            .expect("third acquire primary");

        // Restore env so we don't poison sibling tests.
        drop(_lock2);
        #[cfg(target_os = "macos")]
        unsafe {
            if let Some(h) = original_home {
                std::env::set_var("HOME", h);
            } else {
                std::env::remove_var("HOME");
            }
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        unsafe {
            if let Some(d) = original {
                std::env::set_var("XDG_RUNTIME_DIR", d);
            } else {
                std::env::remove_var("XDG_RUNTIME_DIR");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
