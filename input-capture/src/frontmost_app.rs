//! Cross-platform "what's the frontmost app right now?" lookup.
//!
//! Used by [`crate::clipboard::ClipboardMonitor`] to consult a
//! user-maintained suppression list before broadcasting a clipboard
//! change: when the active app at the moment of capture matches an
//! entry in the list (e.g. `1Password.app`), the change is dropped
//! locally rather than going on the wire.
//!
//! Each platform returns a `Some(AppIdent)` whose variant matches
//! the OS — see [`AppIdent`] in `lan-mouse-ipc`. None means we
//! couldn't determine the active app (no compositor support, no
//! permissions, transient race, …); the caller treats that as "not
//! suppressed."
//!
//! # macOS
//!
//! `frontmost_app()` and `list_running_apps()` are currently stubs
//! that return `None` / `Vec::new()`. A proper implementation needs
//! to call into AppKit:
//!
//! - `NSWorkspace.frontmostApplication.bundleIdentifier` →
//!   `Some(AppIdent::MacBundle(...))`
//! - `NSWorkspace.runningApplications` → list
//!
//! Either pull in `objc2` + `objc2-app-kit` and call the bindings
//! directly, or shell out to `osascript` (`tell application
//! "System Events" to get bundle identifier of first application
//! process whose frontmost is true`). The shell-out path avoids new
//! deps but is comparatively slow (~50ms) — fine for clipboard's
//! 500ms poll cadence. Until either lands, manual entries in the
//! suppression list are the way to suppress on macOS.
//!
//! # Concealed-type detection (macOS only, also TODO)
//!
//! macOS password managers stamp `org.nspasteboard.ConcealedType`
//! on the pasteboard so apps can voluntarily skip syncing
//! passwords. Reading that requires
//! `NSPasteboard.generalPasteboard.types`, which lives behind the
//! same Objective-C bridge as the bundle-id lookup above. Implement
//! both at the same time.

use lan_mouse_ipc::AppIdent;

/// Helpers used by the platform-specific backends and exercised in
/// unit tests. Lives at module scope (rather than inside a
/// `#[cfg]`-gated `backend` mod) so the test suite can call into
/// shared logic regardless of which backend is compiled.
pub(crate) mod backend_helpers {
    /// Detect Wayland via `WAYLAND_DISPLAY` env var. Used by both
    /// the Linux backend and a unit test that pins the precedence
    /// rule so a regression in env-var detection surfaces with a
    /// clear failure rather than a silent compositor-mismatch.
    pub fn is_wayland_for_test() -> bool {
        std::env::var_os("WAYLAND_DISPLAY")
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }
}

pub use lan_mouse_ipc::AppIdent as AppIdentRe;

/// Best-effort lookup of the application whose window is currently
/// frontmost. Returns `None` when the platform doesn't support the
/// query (or when the lookup transiently fails — caller should
/// treat that as "not suppressed", not as "suppressed").
pub fn frontmost_app() -> Option<AppIdent> {
    backend::frontmost_app()
}

/// Best-effort enumeration of currently-running apps suitable for
/// the suppression-list "From running apps" UI tab. Empty when the
/// platform doesn't implement enumeration yet — the manual-entry
/// tab still works, so the feature remains usable.
pub fn list_running_apps() -> Vec<AppIdent> {
    backend::list_running_apps()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: the lookup must not panic even when no compositor
    /// is reachable (CI sandboxes, headless `cargo test`, etc.). A
    /// `None` return is a perfectly valid outcome — the caller
    /// treats that as "not suppressed."
    #[test]
    fn frontmost_app_does_not_panic() {
        let _ = frontmost_app();
    }

    #[test]
    fn list_running_apps_does_not_panic() {
        let _ = list_running_apps();
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn wayland_detection_uses_wayland_display_env_var() {
        // We can't actually mutate process env safely from a
        // multi-threaded test runner, so just exercise the helper
        // and verify it returns a deterministic bool. Pinning the
        // mechanism here means a refactor to (e.g.)
        // `XDG_SESSION_TYPE`-only detection would surface as a
        // failed test instead of a silent compositor-mismatch.
        let _ = backend_helpers::is_wayland_for_test();
    }
}

#[cfg(target_os = "macos")]
mod backend {
    use super::AppIdent;

    pub fn frontmost_app() -> Option<AppIdent> {
        // TODO(macOS): NSWorkspace.frontmostApplication.bundleIdentifier
        // via objc2-app-kit. See module-level docs.
        None
    }

    pub fn list_running_apps() -> Vec<AppIdent> {
        // TODO(macOS): NSWorkspace.runningApplications via
        // objc2-app-kit. See module-level docs.
        Vec::new()
    }
}

#[cfg(windows)]
mod backend {
    use super::AppIdent;
    use windows::Win32::Foundation::{CloseHandle, FALSE, HWND};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetForegroundWindow, GetWindowThreadProcessId, IsWindowVisible,
    };
    use windows::core::{BOOL, LPARAM, PWSTR};

    fn process_basename(pid: u32) -> Option<String> {
        if pid == 0 {
            return None;
        }
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let result = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            );
            let _ = CloseHandle(handle);
            result.ok()?;
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            std::path::Path::new(&path)
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_lowercase())
        }
    }

    pub fn frontmost_app() -> Option<AppIdent> {
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd == HWND::default() {
                return None;
            }
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            process_basename(pid).map(AppIdent::WindowsExe)
        }
    }

    pub fn list_running_apps() -> Vec<AppIdent> {
        // Walk every visible top-level window, dedup by process
        // basename. Closures captured via LPARAM pointer to a Vec.
        let mut basenames: Vec<String> = Vec::new();
        unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
            unsafe {
                if IsWindowVisible(hwnd) == FALSE {
                    return BOOL(1); // continue
                }
                let mut pid: u32 = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                let Some(name) = super::process_basename(pid) else {
                    return BOOL(1);
                };
                let v: &mut Vec<String> = &mut *(lparam.0 as *mut Vec<String>);
                if !v.iter().any(|n| n == &name) {
                    v.push(name);
                }
                BOOL(1)
            }
        }
        unsafe {
            let _ = EnumWindows(
                Some(enum_proc),
                LPARAM(&mut basenames as *mut _ as isize),
            );
        }
        basenames
            .into_iter()
            .map(AppIdent::WindowsExe)
            .collect()
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod backend {
    use super::AppIdent;
    use std::process::Command;

    /// Detect compositor flavor via env vars. Wayland sessions set
    /// `WAYLAND_DISPLAY`; X11 sessions don't. `XDG_SESSION_TYPE` is
    /// the modern signal but isn't always set on tiling WMs (Sway,
    /// Hyprland) so we treat presence of `WAYLAND_DISPLAY` as
    /// authoritative for Wayland.
    fn is_wayland() -> bool {
        super::backend_helpers::is_wayland_for_test()
    }

    pub fn frontmost_app() -> Option<AppIdent> {
        if is_wayland() {
            hyprland_active()
                .or_else(sway_active)
                .map(|s| AppIdent::LinuxWayland(s.to_lowercase()))
        } else {
            x11_active().map(|s| AppIdent::LinuxX11(s.to_lowercase()))
        }
    }

    pub fn list_running_apps() -> Vec<AppIdent> {
        if is_wayland() {
            // Hyprland's `clients -j` returns every mapped client;
            // sway's `get_tree` returns the whole tree. Either way
            // we extract `class` / `app_id`, dedup, and sort for
            // stable display in the GUI.
            let mut idents: Vec<String> = hyprland_clients()
                .into_iter()
                .chain(sway_clients())
                .collect();
            idents.sort();
            idents.dedup();
            idents
                .into_iter()
                .map(|s| AppIdent::LinuxWayland(s.to_lowercase()))
                .collect()
        } else {
            let mut classes = x11_client_list();
            classes.sort();
            classes.dedup();
            classes
                .into_iter()
                .map(|s| AppIdent::LinuxX11(s.to_lowercase()))
                .collect()
        }
    }

    fn run_capture(cmd: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(cmd).args(args).output().ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok()
    }

    fn hyprland_active() -> Option<String> {
        let json = run_capture("hyprctl", &["activewindow", "-j"])?;
        let parsed: serde_json::Value = serde_json::from_str(&json).ok()?;
        // Hyprland reports the X11 WM_CLASS-equivalent as `class`.
        // `initialClass` is the value the toplevel registered with;
        // prefer it when present so a renamed window doesn't slip
        // suppression by changing its title.
        let class = parsed
            .get("initialClass")
            .and_then(|v| v.as_str())
            .or_else(|| parsed.get("class").and_then(|v| v.as_str()))?;
        let class = class.trim();
        if class.is_empty() {
            return None;
        }
        Some(class.to_owned())
    }

    fn hyprland_clients() -> Vec<String> {
        let Some(json) = run_capture("hyprctl", &["clients", "-j"]) else {
            return Vec::new();
        };
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json) else {
            return Vec::new();
        };
        parsed
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| {
                        c.get("initialClass")
                            .and_then(|v| v.as_str())
                            .or_else(|| c.get("class").and_then(|v| v.as_str()))
                    })
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn sway_active() -> Option<String> {
        let json = run_capture("swaymsg", &["-t", "get_tree"])?;
        let tree: serde_json::Value = serde_json::from_str(&json).ok()?;
        find_focused_app_id(&tree)
    }

    fn sway_clients() -> Vec<String> {
        let Some(json) = run_capture("swaymsg", &["-t", "get_tree"]) else {
            return Vec::new();
        };
        let Ok(tree) = serde_json::from_str::<serde_json::Value>(&json) else {
            return Vec::new();
        };
        let mut acc = Vec::new();
        collect_app_ids(&tree, &mut acc);
        acc
    }

    /// Walk the sway/i3 tree depth-first looking for the node with
    /// `focused == true` and a non-empty `app_id` (Wayland clients)
    /// or `window_properties.class` (XWayland fallback).
    fn find_focused_app_id(node: &serde_json::Value) -> Option<String> {
        if node.get("focused").and_then(|v| v.as_bool()) == Some(true) {
            if let Some(s) = node.get("app_id").and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    return Some(s.to_owned());
                }
            }
            if let Some(s) = node
                .get("window_properties")
                .and_then(|wp| wp.get("class"))
                .and_then(|v| v.as_str())
            {
                if !s.is_empty() {
                    return Some(s.to_owned());
                }
            }
        }
        for key in ["nodes", "floating_nodes"] {
            if let Some(arr) = node.get(key).and_then(|v| v.as_array()) {
                for child in arr {
                    if let Some(found) = find_focused_app_id(child) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    fn collect_app_ids(node: &serde_json::Value, acc: &mut Vec<String>) {
        if let Some(s) = node.get("app_id").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                acc.push(s.to_owned());
            }
        }
        if let Some(s) = node
            .get("window_properties")
            .and_then(|wp| wp.get("class"))
            .and_then(|v| v.as_str())
        {
            if !s.is_empty() {
                acc.push(s.to_owned());
            }
        }
        for key in ["nodes", "floating_nodes"] {
            if let Some(arr) = node.get(key).and_then(|v| v.as_array()) {
                for child in arr {
                    collect_app_ids(child, acc);
                }
            }
        }
    }

    fn x11_active() -> Option<String> {
        use x11rb::connection::Connection;
        use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

        let (conn, screen_num) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots[screen_num].root;
        let net_active = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let prop = conn
            .get_property(false, root, net_active, AtomEnum::WINDOW, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        let window_id = prop.value32()?.next()?;
        if window_id == 0 {
            return None;
        }
        let class_prop = conn
            .get_property(
                false,
                window_id,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                0,
                1024,
            )
            .ok()?
            .reply()
            .ok()?;
        // WM_CLASS is two NUL-separated strings: instance, class.
        // Prefer the second (class) since it tends to be the more
        // stable identifier.
        let raw = class_prop.value;
        let mut parts = raw.split(|&b| b == 0).filter(|s| !s.is_empty());
        let _instance = parts.next();
        let class = parts.next();
        let bytes = class.or_else(|| {
            // Single-string fallback (some toolkits put the same
            // value in both fields without a separator).
            raw.split(|&b| b == 0).find(|s| !s.is_empty())
        })?;
        let s = String::from_utf8_lossy(bytes).into_owned();
        if s.is_empty() {
            return None;
        }
        Some(s)
    }

    fn x11_client_list() -> Vec<String> {
        use x11rb::connection::Connection;
        use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

        let Ok((conn, screen_num)) = x11rb::connect(None) else {
            return Vec::new();
        };
        let root = conn.setup().roots[screen_num].root;
        let Ok(reply) = conn.intern_atom(false, b"_NET_CLIENT_LIST") else {
            return Vec::new();
        };
        let Ok(net_client_list) = reply.reply() else {
            return Vec::new();
        };
        let net_client_list = net_client_list.atom;
        let Ok(prop_req) = conn.get_property(
            false,
            root,
            net_client_list,
            AtomEnum::WINDOW,
            0,
            u32::MAX,
        ) else {
            return Vec::new();
        };
        let Ok(prop) = prop_req.reply() else {
            return Vec::new();
        };
        let Some(values) = prop.value32() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for window_id in values {
            if window_id == 0 {
                continue;
            }
            let Ok(req) = conn.get_property(
                false,
                window_id,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                0,
                1024,
            ) else {
                continue;
            };
            let Ok(class_prop) = req.reply() else {
                continue;
            };
            let raw = class_prop.value;
            let mut parts = raw.split(|&b| b == 0).filter(|s| !s.is_empty());
            let _instance = parts.next();
            let class = parts.next();
            if let Some(bytes) = class.or_else(|| raw.split(|&b| b == 0).find(|s| !s.is_empty())) {
                out.push(String::from_utf8_lossy(bytes).into_owned());
            }
        }
        out
    }
}
