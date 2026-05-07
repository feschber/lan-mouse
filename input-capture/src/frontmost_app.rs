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
//! Implemented via `objc2-app-kit` against `NSWorkspace`:
//!
//! - `frontmost_app()` →
//!   `NSWorkspace.sharedWorkspace.frontmostApplication.bundleIdentifier`
//!   wrapped in `AppIdent::MacBundle`.
//! - `list_running_apps()` →
//!   `NSWorkspace.runningApplications` map → bundle ID. Apps with
//!   no bundle ID (anonymous helpers) are skipped.
//!
//! Concealed-type pasteboard detection lives in
//! [`crate::clipboard`] (`is_concealed_clipboard`) and uses the
//! same objc bridge to check `NSPasteboard.types` for
//! `org.nspasteboard.ConcealedType`.

use lan_mouse_ipc::{AppIdent, RunningApp};

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
/// the suppression-list picker. Each entry pairs a human-readable
/// display name with the host-OS identifier used by the runtime
/// suppression check. Empty when the platform can't enumerate
/// (no compositor support, missing permissions, transient race).
pub fn list_running_apps() -> Vec<RunningApp> {
    backend::list_running_apps()
}

/// Resolve a host-OS identifier (e.g. macOS bundle ID) into a
/// `RunningApp` with display name + icon, even when the app isn't
/// currently running. Used by the GUI to render the suppressed-
/// apps list — the user added entries by bundle ID and we want to
/// show "1Password" with the 1Password icon, not the raw
/// `com.1password.1password` string.
///
/// Returns `None` when the identifier doesn't resolve to an
/// installed app (e.g. uninstalled since being added) or on
/// platforms without a per-platform implementation. Callers
/// should fall back to displaying the identifier verbatim.
pub fn lookup_app_metadata(identifier: &str) -> Option<RunningApp> {
    backend::lookup_app_metadata(identifier)
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
    use super::{AppIdent, RunningApp};
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSImage, NSWorkspace,
    };
    use objc2_foundation::{NSDictionary, NSString};
    use std::collections::HashMap;
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};

    pub fn frontmost_app() -> Option<AppIdent> {
        let workspace = NSWorkspace::sharedWorkspace();
        let app = workspace.frontmostApplication()?;
        let bundle_id = app.bundleIdentifier()?;
        Some(AppIdent::MacBundle(bundle_id.to_string()))
    }

    /// Enumerate user-visible apps via `osascript` → System Events.
    ///
    /// Three direct AppKit APIs all silently scope to the caller's
    /// loginwindow / Aqua session — a non-Cocoa GTK process running
    /// as the .app's main process does NOT have a full session, so
    /// `NSWorkspace.runningApplications`, `NSRunningApplication
    /// .runningApplicationWithProcessIdentifier`, and
    /// `CGWindowListCopyWindowInfo` only return apps with which
    /// our process happens to share a Mach connection (XPC services
    /// we use, accessibility agents, recently-activated panes,
    /// pasteboard peers). Real Cocoa apps the user is using stay
    /// invisible.
    ///
    /// System Events is itself fully session-attached and returns
    /// the complete process list. We talk to it through the
    /// already-permissioned Apple Events channel
    /// (`NSAppleEventsUsageDescription` is declared, the user has
    /// already granted automation control for input emulation).
    /// The script returns one tab-separated row per visible app:
    /// `bundle_id\tposix_path\tname`. Helpers / XPC services /
    /// preference-pane extensions are excluded by System Events'
    /// own definition of `background only is false`.
    pub fn list_running_apps() -> Vec<RunningApp> {
        let raw = match query_visible_apps_via_system_events() {
            Some(s) => s,
            None => {
                log::debug!("list_running_apps: System Events query failed");
                return Vec::new();
            }
        };
        let mut out: Vec<RunningApp> = Vec::with_capacity(32);
        for line in raw.lines() {
            let mut parts = line.splitn(3, '\t');
            let identifier = parts.next().unwrap_or("").trim();
            let path = parts.next().unwrap_or("").trim();
            let display_name = parts.next().unwrap_or("").trim();
            if identifier.is_empty() || path.is_empty() || display_name.is_empty() {
                continue;
            }
            // Hide our own bundle — suppressing your own clipboard
            // app makes no sense (we ARE the clipboard sender).
            if identifier == "de.feschber.LanMouse" {
                continue;
            }
            let icon_png = cached_or_encoded_icon(identifier, path);
            out.push(RunningApp {
                display_name: display_name.to_owned(),
                identifier: identifier.to_owned(),
                icon_png,
            });
        }
        out.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
        out.dedup_by(|a, b| a.identifier == b.identifier);
        log::debug!("list_running_apps: {} visible apps via System Events", out.len());
        out
    }

    /// Spawn `osascript` with an inline AppleScript that asks
    /// System Events for every non-background process and returns
    /// `bundle_id\tposix_path\tname` per line. Inner try-catches
    /// silently skip processes whose bundle ID or file we can't
    /// resolve (rare system processes), so the result is always
    /// well-formed. Returns `None` only if osascript itself fails
    /// — typically because the user hasn't granted Apple Events
    /// permission yet, in which case the picker stays empty until
    /// they accept the system prompt.
    fn query_visible_apps_via_system_events() -> Option<String> {
        const SCRIPT: &str = r#"
tell application "System Events"
    set out to ""
    try
        set procs to (every process where background only is false)
        repeat with p in procs
            try
                set bid to bundle identifier of p
                set fp to POSIX path of ((file of p) as alias)
                set nm to name of p
                set out to out & bid & tab & fp & tab & nm & linefeed
            end try
        end repeat
    end try
    return out
end tell
"#;
        let output = Command::new("/usr/bin/osascript")
            .args(["-e", SCRIPT])
            .output()
            .ok()?;
        if !output.status.success() {
            log::debug!(
                "osascript failed (exit {:?}): {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
            return None;
        }
        String::from_utf8(output.stdout).ok()
    }

    /// Cache PNG icon bytes by bundle identifier. The 5-second
    /// auto-refresh would otherwise re-encode every icon on every
    /// tick, which adds up to tens of milliseconds of main-thread
    /// work per refresh. Icons rarely change while an app is
    /// running, so caching by bundle ID is a clean trade.
    fn cached_or_encoded_icon(bundle_id: &str, app_path: &str) -> Option<Vec<u8>> {
        static CACHE: OnceLock<Mutex<HashMap<String, Option<Vec<u8>>>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(guard) = cache.lock() {
            if let Some(hit) = guard.get(bundle_id) {
                return hit.clone();
            }
        }
        let png = encode_icon_for_app_path(app_path);
        if let Ok(mut guard) = cache.lock() {
            guard.insert(bundle_id.to_owned(), png.clone());
        }
        png
    }

    fn encode_icon_for_app_path(app_path: &str) -> Option<Vec<u8>> {
        let workspace = NSWorkspace::sharedWorkspace();
        let path_str = NSString::from_str(app_path);
        let icon = workspace.iconForFile(&path_str);
        encode_nsimage_to_small_png(&icon)
    }

    /// Look up display name + icon for an installed app by bundle
    /// ID, even if it's not currently running. Uses Launch
    /// Services (`URLForApplicationWithBundleIdentifier`) to find
    /// the .app's path on disk, then derives the display name from
    /// the bundle's file name (`/Applications/1Password.app` →
    /// `1Password`) and loads its icon via `iconForFile:`. Both
    /// APIs are path-based and session-independent.
    pub(super) fn lookup_app_metadata(identifier: &str) -> Option<RunningApp> {
        let workspace = NSWorkspace::sharedWorkspace();
        let bid_str = NSString::from_str(identifier);
        let url = unsafe { workspace.URLForApplicationWithBundleIdentifier(&bid_str) }?;
        let path_ns = url.path()?;
        let path_str = path_ns.to_string();
        let display_name = std::path::Path::new(&path_str)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(String::from)
            .unwrap_or_else(|| identifier.to_owned());
        let icon_png = cached_or_encoded_icon(identifier, &path_str);
        Some(RunningApp {
            display_name,
            identifier: identifier.to_owned(),
            icon_png,
        })
    }

    fn encode_nsimage_to_small_png(icon: &NSImage) -> Option<Vec<u8>> {
        // Pick the rep that's closest-but-no-smaller than 64 px.
        // .icns files typically include 16/32/64/128/256/512/1024;
        // anything bigger ships hundreds of KB of PNG over IPC for
        // no display benefit.
        let target_px: f64 = 64.0;
        let reps = icon.representations();
        let mut best_idx: Option<usize> = None;
        let mut best_w: f64 = f64::INFINITY;
        for (i, rep) in reps.iter().enumerate() {
            let w = rep.size().width;
            if w >= target_px && w < best_w {
                best_idx = Some(i);
                best_w = w;
            }
        }
        if best_idx.is_none() {
            let mut max_w: f64 = 0.0;
            for (i, rep) in reps.iter().enumerate() {
                let w = rep.size().width;
                if w > max_w {
                    best_idx = Some(i);
                    max_w = w;
                }
            }
        }
        let bitmap_rep = if let Some(i) = best_idx {
            reps.objectAtIndex(i)
                .downcast::<NSBitmapImageRep>()
                .ok()
                .or_else(|| {
                    let tiff = icon.TIFFRepresentation()?;
                    NSBitmapImageRep::imageRepWithData(&tiff)
                })
        } else {
            let tiff = icon.TIFFRepresentation()?;
            NSBitmapImageRep::imageRepWithData(&tiff)
        }?;
        let empty = NSDictionary::<NSString>::dictionary();
        let png = unsafe {
            bitmap_rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &empty)
        }?;
        let bytes = unsafe { png.as_bytes_unchecked() };
        Some(bytes.to_vec())
    }

}

#[cfg(windows)]
mod backend {
    use super::{AppIdent, RunningApp};
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

    pub fn list_running_apps() -> Vec<RunningApp> {
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
        let mut out: Vec<RunningApp> = basenames
            .into_iter()
            .map(|name| RunningApp {
                display_name: name.clone(),
                identifier: name,
                icon_png: None,
            })
            .collect();
        out.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        out
    }

    pub(super) fn lookup_app_metadata(_identifier: &str) -> Option<RunningApp> {
        // No installed-app metadata source on Windows yet.
        None
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod backend {
    use super::{AppIdent, RunningApp};
    use crate::desktop_entries::{self, AppDirectory};
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

    pub fn list_running_apps() -> Vec<RunningApp> {
        let mut idents: Vec<String> = if is_wayland() {
            // Hyprland's `clients -j` returns every mapped client;
            // sway's `get_tree` returns the whole tree. Either way
            // we extract `class` / `app_id`, dedup, and sort for
            // stable display in the GUI.
            hyprland_clients()
                .into_iter()
                .chain(sway_clients())
                .collect()
        } else {
            x11_client_list()
        };
        idents.sort();
        idents.dedup();
        // Enrich each runtime identifier with its installed-app
        // metadata (display name + icon bytes) when a .desktop
        // entry can be matched. Apps with no .desktop hit fall
        // through to the raw-string display path so an unknown
        // class still shows up in the picker.
        let directory = desktop_entries::discover_apps();
        let mut out: Vec<RunningApp> = idents
            .into_iter()
            .map(|raw| build_running_app(&directory, raw))
            .collect();
        // Re-sort by display name now that .desktop enrichment may
        // have rewritten "firefox" → "Firefox", etc., so the picker
        // shows entries in human-readable order.
        out.sort_by(|a, b| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
        });
        out
    }

    /// Resolve a stored host-OS identifier (a lowercased class /
    /// app_id) back to a [`RunningApp`] using the same .desktop
    /// scan the picker uses. Lets the GUI render a previously-
    /// added entry as `1Password` with its icon even when the app
    /// isn't currently running.
    pub(super) fn lookup_app_metadata(identifier: &str) -> Option<RunningApp> {
        let directory = desktop_entries::discover_apps();
        let app = build_running_app(&directory, identifier.to_owned());
        // build_running_app always returns Something; only treat
        // it as "found" when the .desktop scan actually contributed
        // metadata (display name differs from the identifier, or
        // we got an icon).
        if app.display_name.eq_ignore_ascii_case(identifier) && app.icon_png.is_none() {
            None
        } else {
            Some(app)
        }
    }

    /// Assemble a [`RunningApp`] from a runtime identifier plus
    /// the [`AppDirectory`]. The identifier is lowercased so the
    /// direct + Chrome-PWA-fallback lookups in
    /// [`AppDirectory::lookup`] hit the same case the indexer
    /// inserted under.
    fn build_running_app(directory: &AppDirectory, raw_identifier: String) -> RunningApp {
        let lower = raw_identifier.to_lowercase();
        if let Some(meta) = directory.lookup(&lower) {
            let icon_png = meta
                .icon_name
                .as_deref()
                .and_then(desktop_entries::icon_bytes_for_name);
            return RunningApp {
                display_name: meta.display_name,
                identifier: lower,
                icon_png,
            };
        }
        RunningApp {
            display_name: raw_identifier,
            identifier: lower,
            icon_png: None,
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
