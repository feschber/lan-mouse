//! Discover and parse freedesktop `.desktop` files for the
//! clipboard-suppression picker on Linux.
//!
//! Two responsibilities:
//!
//! 1. Build a map from a runtime identifier (Hyprland `class`,
//!    Sway `app_id`, X11 `WM_CLASS` — all lowercased) to a
//!    [`DesktopAppMetadata`] record. The map is keyed both by the
//!    `.desktop` filename stem and by `StartupWMClass=` so the
//!    common cases — `firefox.desktop` matching a `firefox` class
//!    and `1password.desktop` (StartupWMClass=`1Password`) matching
//!    a `1Password` class — both resolve.
//!
//! 2. Resolve a freedesktop icon *name* (e.g. `firefox`) into
//!    raster bytes that GTK can load via `gdk::Texture::from_bytes`.
//!    PNG is preferred; SVG falls through to gdk-pixbuf's librsvg
//!    loader on the GTK side. The picker target is ~64–128 px so
//!    we prefer those sizes and degrade gracefully when only
//!    larger or scalable variants exist.
//!
//! Scope is intentionally narrow: this module exists to make the
//! suppression-list modal show "Firefox" with its real icon
//! instead of `firefox` as bare text. It does NOT replace the
//! runtime suppression check itself, which still keys on
//! [`crate::frontmost_app::frontmost_app`] returning a host-OS
//! identifier.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// One installed application learned from a `.desktop` file.
#[derive(Debug, Clone)]
pub struct DesktopAppMetadata {
    /// `Name=` field — the human-readable display name. Falls back
    /// to the .desktop filename stem when `Name=` is absent or
    /// empty.
    pub display_name: String,
    /// `Icon=` field. May be a bare freedesktop icon name (typical:
    /// `firefox`) or an absolute path. `None` if the .desktop file
    /// has no `Icon=` line or the value is empty.
    pub icon_name: Option<String>,
}

/// Result of [`discover_apps`]: two indices over the same set of
/// installed `.desktop` entries.
///
/// - `by_identifier`: keyed by lowercased filename stem AND
///   `StartupWMClass` so a runtime class / app_id resolves directly
///   to its installed metadata (the common case).
/// - `by_webapp_host`: keyed by lowercased hostname extracted from
///   any `https?://…` token in the entry's `Exec=` line. Used to
///   resolve Chrome / Chromium `--app=URL` PWAs whose runtime class
///   is `chrome-<host>__<path>-<Profile>`. The omarchy
///   `omarchy-launch-webapp <url>` flow falls into this bucket: the
///   `.desktop` has Name + Icon + Exec=URL but no `StartupWMClass`,
///   so the direct index can't see it.
#[derive(Debug, Default)]
pub struct AppDirectory {
    pub by_identifier: HashMap<String, DesktopAppMetadata>,
    pub by_webapp_host: HashMap<String, DesktopAppMetadata>,
}

impl AppDirectory {
    /// Resolve a runtime class / app_id to its installed metadata.
    /// Tries the direct index first, then falls back to parsing
    /// the class as a Chrome `--app=` PWA and matching the host
    /// against `by_webapp_host`. Returns `None` when nothing
    /// matches; the caller renders the raw identifier as the
    /// display name.
    pub fn lookup(&self, identifier: &str) -> Option<DesktopAppMetadata> {
        let lower = identifier.to_lowercase();
        if let Some(m) = self.by_identifier.get(&lower) {
            return Some(m.clone());
        }
        if let Some(host) = parse_chrome_pwa_host(&lower) {
            if let Some(m) = self.by_webapp_host.get(host) {
                return Some(m.clone());
            }
        }
        None
    }
}

/// Scan every standard `.desktop` location, returning the two-way
/// [`AppDirectory`]. Apps with `Type != Application` /
/// `Hidden=true` / `NoDisplay=true` are dropped so the picker
/// doesn't fill up with `xdg-open`-style helper entries the user
/// can't actually focus.
pub fn discover_apps() -> AppDirectory {
    let mut by_identifier: HashMap<String, DesktopAppMetadata> = HashMap::new();
    let mut by_webapp_host: HashMap<String, DesktopAppMetadata> = HashMap::new();
    for dir in standard_app_dirs() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("desktop") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(contents) = fs::read_to_string(&path) else {
                continue;
            };
            let Some(parsed) = parse_desktop_entry(&contents) else {
                continue;
            };
            let display_name = parsed
                .name
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| stem.to_owned());
            let metadata = DesktopAppMetadata {
                display_name,
                icon_name: parsed.icon.filter(|s| !s.is_empty()),
            };
            // Index by .desktop filename stem (matches the common
            // case where WM_CLASS / app_id matches the app's binary
            // name — `firefox.desktop` ↔ `firefox`).
            by_identifier
                .entry(stem.to_lowercase())
                .or_insert_with(|| metadata.clone());
            // ALSO index by StartupWMClass when present — that's
            // the explicit hint the .desktop author published for
            // matching against window classes that disagree with
            // the filename stem (`1password.desktop` →
            // `StartupWMClass=1Password`).
            if let Some(wmclass) = parsed.startup_wm_class.as_deref().filter(|s| !s.is_empty()) {
                by_identifier
                    .entry(wmclass.to_lowercase())
                    .or_insert_with(|| metadata.clone());
            }
            // Webapp index: every host that appears in an http(s)://
            // URL token in the Exec= line. Lets Chrome `--app=URL`
            // PWAs fall back to this entry's Name + Icon when the
            // direct index misses (typical of .desktop files that
            // don't bother setting `StartupWMClass`).
            if let Some(exec) = parsed.exec.as_deref() {
                for host in extract_hosts_from_exec(exec) {
                    by_webapp_host
                        .entry(host)
                        .or_insert_with(|| metadata.clone());
                }
            }
        }
    }
    AppDirectory {
        by_identifier,
        by_webapp_host,
    }
}

/// Resolve an icon name to PNG or SVG bytes. Prefers raster sizes
/// in the 64–128 px window where the picker actually displays them;
/// falls through to scalable SVG and finally to `/usr/share/pixmaps`
/// when the freedesktop hicolor theme doesn't have an entry.
///
/// Absolute paths bypass the search and read directly. Returns
/// `None` when no matching file is found or the read fails.
pub fn icon_bytes_for_name(icon_name: &str) -> Option<Vec<u8>> {
    if icon_name.is_empty() {
        return None;
    }
    // Absolute path → just read it.
    let direct = Path::new(icon_name);
    if direct.is_absolute() {
        return fs::read(direct).ok();
    }
    // Preferred raster sizes, picker-friendly first. Larger sizes
    // serve HiDPI; smaller are the last raster fallback before SVG.
    const RASTER_SIZES: &[&str] = &[
        "128x128", "256x256", "64x64", "96x96", "192x192", "48x48", "32x32",
    ];
    for base in icon_search_dirs() {
        for size in RASTER_SIZES {
            let p = base
                .join(size)
                .join("apps")
                .join(format!("{icon_name}.png"));
            if let Ok(bytes) = fs::read(&p) {
                return Some(bytes);
            }
        }
        // Scalable (SVG) fallback. gdk-pixbuf with librsvg loaded
        // can render this directly via gdk::Texture::from_bytes.
        let svg = base
            .join("scalable")
            .join("apps")
            .join(format!("{icon_name}.svg"));
        if let Ok(bytes) = fs::read(&svg) {
            return Some(bytes);
        }
    }
    // /usr/share/pixmaps/<name>.{png,svg} as a final fallback —
    // the legacy "no theme" icon directory.
    for ext in ["png", "svg"] {
        let p = PathBuf::from("/usr/share/pixmaps").join(format!("{icon_name}.{ext}"));
        if let Ok(bytes) = fs::read(&p) {
            return Some(bytes);
        }
    }
    None
}

/// Application directories per the XDG Base Directory spec, in
/// lookup-priority order: user-local first, then system. Apps in
/// later directories are silently shadowed by earlier ones with
/// matching `.desktop` filenames (`HashMap::entry().or_insert_with`).
fn standard_app_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(home).join("applications"));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty());
    let data_dirs = data_dirs.unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    for d in data_dirs.split(':').filter(|s| !s.is_empty()) {
        dirs.push(PathBuf::from(d).join("applications"));
    }
    // Flatpak system & user exports — these aren't always present
    // in $XDG_DATA_DIRS depending on distro / flatpak version.
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(&home).join(".local/share/flatpak/exports/share/applications"));
    }
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    dirs
}

/// Hicolor theme search roots. We don't consult the user's selected
/// theme on purpose — the suppression picker works just fine with
/// the universal hicolor fallback, and per-theme lookup adds cost
/// (parse `index.theme`, walk inheritance) that doesn't pay back
/// for a one-shot list of apps.
fn icon_search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(home).join("icons/hicolor"));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/icons/hicolor"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty());
    let data_dirs = data_dirs.unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    for d in data_dirs.split(':').filter(|s| !s.is_empty()) {
        dirs.push(PathBuf::from(d).join("icons/hicolor"));
    }
    dirs
}

#[derive(Default, Debug)]
struct ParsedDesktopEntry {
    name: Option<String>,
    icon: Option<String>,
    startup_wm_class: Option<String>,
    exec: Option<String>,
}

/// Parse the `[Desktop Entry]` section of a `.desktop` file. Stops
/// at the first blank line or at the first non-`[Desktop Entry]`
/// section header — we don't need locale-specific `Name[xx]=`
/// variants for the picker's English-only display today.
///
/// Returns `None` when the entry is `Type=Application`-incompatible
/// (anything other than Application, including missing Type),
/// `Hidden=true`, or `NoDisplay=true`. The caller treats that as
/// "skip this app" rather than rendering an unfocusable shell.
fn parse_desktop_entry(contents: &str) -> Option<ParsedDesktopEntry> {
    let mut in_section = false;
    let mut entry = ParsedDesktopEntry::default();
    let mut entry_type: Option<String> = None;
    let mut hidden = false;
    let mut no_display = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            // First [Desktop Entry] header switches us in; any other
            // header (e.g. [Desktop Action xyz]) ends parsing for our
            // purposes.
            in_section = line == "[Desktop Entry]";
            if !in_section {
                break;
            }
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "Name" => entry.name = Some(value.to_owned()),
            "Icon" => entry.icon = Some(value.to_owned()),
            "StartupWMClass" => entry.startup_wm_class = Some(value.to_owned()),
            "Exec" => entry.exec = Some(value.to_owned()),
            "Type" => entry_type = Some(value.to_owned()),
            "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
            "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    if entry_type.as_deref() != Some("Application") || hidden || no_display {
        return None;
    }
    Some(entry)
}

/// Extract the host portion of every `https?://…` token in an
/// `Exec=` line. Hosts are lowercased to match Chrome's class-
/// derivation convention. A single Exec line can have multiple
/// URLs (rare but legal); we capture them all so a future tab/PWA
/// launch with any of those URLs still resolves.
fn extract_hosts_from_exec(exec: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    for token in exec.split_whitespace() {
        // Strip surrounding quotes (Exec= can have quoted args).
        let token = token.trim_matches(|c| c == '"' || c == '\'');
        let after = match token
            .strip_prefix("https://")
            .or_else(|| token.strip_prefix("http://"))
        {
            Some(s) => s,
            None => continue,
        };
        // Host runs until the first /, ?, #, : (port), or
        // end-of-token.
        let host_end = after.find(['/', '?', '#', ':']).unwrap_or(after.len());
        let host = &after[..host_end];
        if host.is_empty() || !host.contains('.') {
            continue;
        }
        hosts.push(host.to_lowercase());
    }
    hosts
}

/// Parse a Chrome / Chromium `--app=URL` window class string and
/// return the host portion of the URL it was derived from.
///
/// Chrome's class encoding for `--app=https://<host>/<path>` is
/// `chrome-<host>__<path-with-/-replaced-by-_>-<Profile>`. The
/// `__` always separates host from path; everything before it is
/// the host. For URLs without a path
/// (`--app=https://<host>/`), the class collapses to
/// `chrome-<host>-<Profile>` so we also strip a trailing
/// `-default` / `-profile_N` segment as a fallback.
///
/// Returns `None` for non-Chrome classes, classes whose host has
/// no `.` (almost certainly an extension ID — `crx_…` /
/// `chrome-<extension-id>-default` already get matched by the
/// direct .desktop index, so the webapp fallback is redundant for
/// them), or empty hosts.
fn parse_chrome_pwa_host(class_lowercased: &str) -> Option<&str> {
    let after = class_lowercased.strip_prefix("chrome-")?;
    // Path-bearing form: `<host>__<path>-<profile>`. The path
    // separator is unambiguous so we stop at the first `__`.
    let host = if let Some(idx) = after.find("__") {
        &after[..idx]
    } else {
        // Path-less form: `<host>-<profile>`. Strip a single
        // known profile suffix; default and `profile_N` cover the
        // standard Chrome multi-profile setup. `-default` is also
        // what Brave / Vivaldi / Edge use.
        let mut trimmed = after;
        for suffix in [
            "-default",
            "-profile_1",
            "-profile_2",
            "-profile_3",
            "-profile_4",
        ] {
            if let Some(s) = trimmed.strip_suffix(suffix) {
                trimmed = s;
                break;
            }
        }
        // If we couldn't identify a profile suffix the class is
        // probably not a webapp — bail.
        if trimmed == after {
            return None;
        }
        trimmed
    };
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    Some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_application_entry() {
        let raw = "[Desktop Entry]\nName=Firefox\nIcon=firefox\nType=Application\n";
        let parsed = parse_desktop_entry(raw).expect("application entry");
        assert_eq!(parsed.name.as_deref(), Some("Firefox"));
        assert_eq!(parsed.icon.as_deref(), Some("firefox"));
        assert_eq!(parsed.startup_wm_class, None);
    }

    #[test]
    fn captures_startup_wm_class() {
        let raw = "[Desktop Entry]\nName=1Password\nIcon=1password\n\
                   Type=Application\nStartupWMClass=1Password\n";
        let parsed = parse_desktop_entry(raw).expect("application entry");
        assert_eq!(parsed.startup_wm_class.as_deref(), Some("1Password"));
    }

    #[test]
    fn rejects_link_type() {
        let raw = "[Desktop Entry]\nName=Some Bookmark\nType=Link\nURL=https://x/\n";
        assert!(parse_desktop_entry(raw).is_none());
    }

    #[test]
    fn rejects_missing_type() {
        // Missing Type= is equivalent to "this isn't an
        // Application", so the picker should skip it.
        let raw = "[Desktop Entry]\nName=Whatever\nIcon=x\n";
        assert!(parse_desktop_entry(raw).is_none());
    }

    #[test]
    fn rejects_hidden_entry() {
        let raw = "[Desktop Entry]\nName=Hidden\nIcon=x\nType=Application\nHidden=true\n";
        assert!(parse_desktop_entry(raw).is_none());
    }

    #[test]
    fn rejects_no_display_entry() {
        let raw = "[Desktop Entry]\nName=Helper\nIcon=x\nType=Application\nNoDisplay=true\n";
        assert!(parse_desktop_entry(raw).is_none());
    }

    #[test]
    fn stops_at_subsequent_section_header() {
        // Locale-specific Name[de_DE]= keys are interleaved between
        // [Desktop Entry] and [Desktop Action xyz] headers in some
        // .desktop files. We only care about the primary section.
        let raw = "[Desktop Entry]\nName=Foo\nType=Application\n\
                   [Desktop Action New]\nName=New Window\n";
        let parsed = parse_desktop_entry(raw).unwrap();
        assert_eq!(parsed.name.as_deref(), Some("Foo"));
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let raw = "# leading comment\n\n[Desktop Entry]\n# inside\nName=Foo\n\nType=Application\n";
        let parsed = parse_desktop_entry(raw).unwrap();
        assert_eq!(parsed.name.as_deref(), Some("Foo"));
    }

    #[test]
    fn discover_apps_smoke_test() {
        // Best-effort: this test runs anywhere `cargo test` runs, so
        // we only assert the function doesn't panic. On a desktop
        // box it'll typically return dozens of entries; on CI it
        // may be empty.
        let _ = discover_apps();
    }

    #[test]
    fn extract_hosts_handles_simple_https_url() {
        let hosts =
            extract_hosts_from_exec("omarchy-launch-webapp https://discord.com/channels/@me");
        assert_eq!(hosts, vec!["discord.com"]);
    }

    #[test]
    fn extract_hosts_handles_quoted_and_multiple_urls() {
        let hosts = extract_hosts_from_exec(
            "browser \"https://gmail.com/u/0\" https://calendar.google.com/r",
        );
        assert_eq!(hosts, vec!["gmail.com", "calendar.google.com"]);
    }

    #[test]
    fn extract_hosts_skips_non_url_tokens() {
        let hosts = extract_hosts_from_exec("firefox %U");
        assert!(hosts.is_empty());
    }

    #[test]
    fn extract_hosts_strips_port_and_query() {
        let hosts = extract_hosts_from_exec("open https://example.com:8080/path?q=1#frag");
        assert_eq!(hosts, vec!["example.com"]);
    }

    #[test]
    fn parse_chrome_pwa_host_handles_path_form() {
        // The omarchy --app=URL case the user reported: Hyprland
        // class `chrome-discord.com__channels_@me-Default`.
        // (We're matching against the lowercased form.)
        assert_eq!(
            parse_chrome_pwa_host("chrome-discord.com__channels_@me-default"),
            Some("discord.com")
        );
    }

    #[test]
    fn parse_chrome_pwa_host_handles_pathless_form() {
        // `--app=https://example.com/` (no path beyond root) →
        // `chrome-example.com-default`.
        assert_eq!(
            parse_chrome_pwa_host("chrome-example.com-default"),
            Some("example.com")
        );
    }

    #[test]
    fn parse_chrome_pwa_host_handles_alt_profiles() {
        assert_eq!(
            parse_chrome_pwa_host("chrome-example.com__app_-profile_2"),
            Some("example.com")
        );
        assert_eq!(
            parse_chrome_pwa_host("chrome-example.com-profile_1"),
            Some("example.com")
        );
    }

    #[test]
    fn parse_chrome_pwa_host_rejects_non_chrome_classes() {
        assert!(parse_chrome_pwa_host("firefox").is_none());
        assert!(parse_chrome_pwa_host("chromium").is_none());
        // `chrome-` prefix but the trailing portion has no dot, so
        // it's almost certainly an extension ID
        // (`chrome-<id>-default`) — the direct .desktop index
        // already covers those, so the webapp fallback should not
        // claim them.
        assert!(parse_chrome_pwa_host("chrome-mjoklplbddabcmpepnokjaffbmgbkkgg-default").is_none());
    }

    #[test]
    fn parse_chrome_pwa_host_rejects_unknown_profile_suffix() {
        // No `__` and no recognized `-default` / `-profile_N`
        // suffix — could be anything.
        assert!(parse_chrome_pwa_host("chrome-example.com-something").is_none());
    }

    #[test]
    fn app_directory_lookup_falls_back_to_webapp_host() {
        // End-to-end: a .desktop with `Exec=… https://discord.com/…`
        // and no StartupWMClass should still resolve the
        // omarchy `--app=` PWA class via the host index.
        let mut by_id: HashMap<String, DesktopAppMetadata> = HashMap::new();
        let mut by_host: HashMap<String, DesktopAppMetadata> = HashMap::new();
        by_id.insert(
            "discord".to_owned(),
            DesktopAppMetadata {
                display_name: "Discord".into(),
                icon_name: Some("/path/to/discord.png".into()),
            },
        );
        by_host.insert(
            "discord.com".to_owned(),
            DesktopAppMetadata {
                display_name: "Discord".into(),
                icon_name: Some("/path/to/discord.png".into()),
            },
        );
        let dir = AppDirectory {
            by_identifier: by_id,
            by_webapp_host: by_host,
        };
        // Direct hit by stem.
        assert_eq!(
            dir.lookup("discord").map(|m| m.display_name),
            Some("Discord".into())
        );
        // Webapp fallback for the omarchy class.
        assert_eq!(
            dir.lookup("chrome-discord.com__channels_@me-Default")
                .map(|m| m.display_name),
            Some("Discord".into())
        );
        // Unknown class falls through to None.
        assert!(dir.lookup("chrome-unknown.com-default").is_none());
    }

    /// Local-development convenience. Run with
    /// `cargo test -p input-capture -- --ignored --nocapture
    /// discover_apps_dump` to see what the .desktop scanner finds
    /// on the current box. Pinned `#[ignore]` so CI / casual `cargo
    /// test` doesn't print to stdout.
    #[test]
    #[ignore]
    fn discover_apps_dump() {
        let dir = discover_apps();
        println!(
            "discovered {} direct entries, {} webapp hosts",
            dir.by_identifier.len(),
            dir.by_webapp_host.len()
        );
        let mut keys: Vec<&String> = dir.by_identifier.keys().collect();
        keys.sort();
        for k in keys {
            let m = &dir.by_identifier[k];
            println!(
                "  id  {k:40} → name={:?} icon={:?}",
                m.display_name, m.icon_name
            );
        }
        let mut hosts: Vec<&String> = dir.by_webapp_host.keys().collect();
        hosts.sort();
        for h in hosts {
            let m = &dir.by_webapp_host[h];
            println!(
                "  web {h:40} → name={:?} icon={:?}",
                m.display_name, m.icon_name
            );
        }
    }
}
