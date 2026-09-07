//! Tiny macOS Privacy-pane helpers used by the GUI.
//!
//! On macOS 13+, the Accessibility grant transitively confers the
//! listen-only event-tap privilege that Input Monitoring gates and the
//! synthesize-event privilege that Post Event gates, and the bundle
//! typically isn't even listed in those separate panes. So the single
//! user-facing action for any missing-capture or missing-emulation
//! scenario is "re-toggle Accessibility" — we don't route elsewhere.

use std::ffi::{c_uchar, c_void};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, Once};

use gtk::glib;

// Launch Services can briefly retain the old registration after process exit;
// `-n` guarantees the scheduled one-shot starts a fresh process.
const RELAUNCH_AFTER_EXIT_SCRIPT: &str = "while IFS= read -r _; do :; done; exec \"$1\" -n \"$2\"";
static RELAUNCH_SIGNAL: Mutex<Option<UnixStream>> = Mutex::new(None);

// Apple declares `AXIsProcessTrusted` as returning `Boolean` (`unsigned char`),
// NOT C's `bool`. Rust's `bool` has a strict bit pattern (0 or 1) so binding
// a `Boolean`-returning function as `-> bool` is technically UB if Apple ever
// returns a non-canonical true value. Keep these as `c_uchar` and normalize.
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> c_uchar;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> c_uchar;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFAllocatorDefault: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: *const c_void;
    static kCFTypeDictionaryValueCallBacks: *const c_void;
    static kCFBooleanTrue: *const c_void;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        num: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

// kAXTrustedCheckOptionPrompt is a CFStringRef exported from ApplicationServices.
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGRequestListenEventAccess() -> c_uchar;
    fn CGRequestPostEventAccess() -> c_uchar;

    // CFMachPortRef CGEventTapCreate(
    //     CGEventTapLocation tap, CGEventTapPlacement place,
    //     CGEventTapOptions options, CGEventMask eventsOfInterest,
    //     CGEventTapCallBack callback, void *userInfo);
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: *const c_void,
        user_info: *const c_void,
    ) -> *const c_void;
}

pub fn accessibility_granted() -> bool {
    let raw = unsafe { AXIsProcessTrusted() };
    log::debug!("AXIsProcessTrusted() = {raw}");
    raw != 0
}

pub enum AccessibilityChange {
    /// AX was missing at startup and the user has now granted it.
    /// Capture/emulation still need a relaunch to take effect, since
    /// the daemon subprocess already bailed.
    Granted,
    /// AX was granted and the user has now revoked it. Quit immediately
    /// — leaving the process alive with an active CGEventTap at
    /// HeadInsertEventTap can wedge system input (clicks/keys silently
    /// consumed) until the process dies. See
    /// macos-cgeventtap-drop-fallthrough-tcc-revoke skill for the
    /// underlying event-tap-disable footgun.
    Revoked,
}

/// Poll for Accessibility grant/revoke transitions. Starts a 1-second
/// GLib timer that fires `on_change` every time `AXIsProcessTrusted()`
/// flips, and keeps running for the lifetime of the process.
///
/// We rely on polling rather than AXObserver because the AX notification
/// API requires a trusted process to subscribe — the precondition we
/// can't assume. This runs on the GTK main thread (via
/// `timeout_add_seconds_local`).
pub fn watch_accessibility_state<F>(mut on_change: F)
where
    F: FnMut(AccessibilityChange) + 'static,
{
    let mut last = accessibility_granted();
    log::info!("watching Accessibility state (initial = {last})");
    glib::timeout_add_seconds_local(1, move || {
        let current = accessibility_granted();
        if current != last {
            log::info!("Accessibility state flip: {last} -> {current}");
            on_change(if current {
                AccessibilityChange::Granted
            } else {
                AccessibilityChange::Revoked
            });
            last = current;
        }
        glib::ControlFlow::Continue
    });
}

pub fn open_accessibility_settings() {
    open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
}

/// Schedule a fresh instance of the current `.app` bundle.
///
/// The helper waits for this process to exit. When this GUI owns the daemon,
/// that follows the graceful shutdown and reap performed by `main`, so the
/// next instance cannot race its IPC socket. Separately managed daemons are
/// left untouched. The caller should quit only after this returns successfully.
pub fn relaunch_bundle() -> io::Result<()> {
    let bundle = bundle_path(&std::env::current_exe()?)?;
    let mut signal = RELAUNCH_SIGNAL
        .lock()
        .map_err(|_| io::Error::other("relaunch state lock poisoned"))?;
    if signal.is_some() {
        return Ok(());
    }

    // The daemon was spawned before this pair exists, so it cannot keep the
    // write end alive. The OS closes it when this GUI process exits, which
    // releases the helper without relying on a delay or a reusable PID.
    let (read_end, write_end) = UnixStream::pair()?;
    relaunch_command(&bundle, read_end).spawn().map(drop)?;
    *signal = Some(write_end);
    Ok(())
}

fn bundle_path(executable: &Path) -> io::Result<PathBuf> {
    let macos = executable
        .parent()
        .filter(|directory| directory.file_name().is_some_and(|name| name == "MacOS"))
        .ok_or_else(invalid_bundle_path)?;
    let contents = macos
        .parent()
        .filter(|directory| directory.file_name().is_some_and(|name| name == "Contents"))
        .ok_or_else(invalid_bundle_path)?;
    let bundle = contents
        .parent()
        .filter(|directory| {
            directory
                .extension()
                .is_some_and(|extension| extension == "app")
        })
        .ok_or_else(invalid_bundle_path)?;
    Ok(bundle.to_owned())
}

fn invalid_bundle_path() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "executable is not inside <bundle>.app/Contents/MacOS",
    )
}

fn relaunch_command(bundle: &Path, read_end: UnixStream) -> Command {
    relaunch_command_with_opener(Path::new("/usr/bin/open"), bundle, read_end)
}

fn relaunch_command_with_opener(opener: &Path, bundle: &Path, read_end: UnixStream) -> Command {
    let read_end: OwnedFd = read_end.into();
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(RELAUNCH_AFTER_EXIT_SCRIPT)
        .arg("lan-mouse-relaunch")
        .arg(opener)
        .arg(bundle)
        .stdin(Stdio::from(read_end))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Make sure the app appears in System Settings → Privacy → Input Monitoring.
///
/// `CGRequestListenEventAccess()` is *supposed* to register the app in the
/// list (and prompt) on first call, but in practice — particularly after a
/// `tccutil reset ListenEvent <bundle>` — it often silently no-ops and the
/// app never gets added. The reliable way to force registration is to
/// attempt a protected action: create a `CGEventTap`. If permission is
/// missing the call returns null, but the attempt itself causes TCC to add
/// the bundle to the Input Monitoring pane so the user can toggle it on.
/// If permission already exists the tap is created successfully, and we
/// tear it down immediately so it doesn't intercept events.
unsafe fn ensure_listed_in_input_monitoring() {
    let req = CGRequestListenEventAccess();
    log::debug!("CGRequestListenEventAccess() = {req}");
    let cb = input_monitoring_noop_tap_callback as *const c_void;
    // Use kCGSessionEventTap (1), NOT kCGHIDEventTap (0). The HID tap sits
    // below window-server input and requires Accessibility in addition to
    // Input Monitoring, so attempting it when Accessibility isn't granted
    // surfaces an Accessibility prompt as a side effect — which is confusing
    // on top of the real Accessibility prompt we already fire explicitly.
    // The session tap requires only Input Monitoring, so its failure is a
    // clean "Input Monitoring missing" signal that TCC uses to list the
    // bundle under the Input Monitoring pane.
    // kCGHeadInsertEventTap = 0, kCGEventTapOptionListenOnly = 1,
    // mask kCGEventKeyDown = 1 << 10.
    let tap = CGEventTapCreate(1, 0, 1, 1 << 10, cb, std::ptr::null());
    log::debug!("CGEventTapCreate(kCGSessionEventTap) -> {tap:?}");
    if !tap.is_null() {
        CFRelease(tap);
    }
}

extern "C" fn input_monitoring_noop_tap_callback(
    _proxy: *const c_void,
    _ty: u32,
    event: *const c_void,
    _refcon: *const c_void,
) -> *const c_void {
    // Pass through unchanged. This tap is never added to a run loop, so
    // in practice the callback never fires — it exists only so the tap
    // can be created (and the attempt is what forces TCC registration).
    event
}

fn open_url(url: &str) {
    if let Err(e) = Command::new("open").arg(url).spawn() {
        log::warn!("failed to open {url}: {e}");
    }
}

/// One-shot, at GUI startup: if a permission is missing, fire the system
/// prompt. This is where the familiar first-launch "Lan Mouse.app would
/// like to control this computer" alert comes from. Subsequent clicks on
/// the Reenable button use URL-scheme navigation instead, so we never
/// double up alerts on retries.
///
/// Guarded with a `Once` because GApplication::activate can fire more
/// than once in a process (reactivation, window presentation) and we
/// must not re-pop the TCC alert on each activation — that looks like a
/// bug to the user.
pub fn fire_initial_prompts() {
    static FIRED: Once = Once::new();
    FIRED.call_once(fire_initial_prompts_inner);
}

fn fire_initial_prompts_inner() {
    if !accessibility_granted() {
        // When Accessibility isn't granted yet, ONLY fire the Accessibility
        // prompt. Do NOT also try to register Input Monitoring or Post Event
        // — those paths have been observed to surface a second Accessibility
        // dialog on top of the one we fire explicitly (Post Event is part of
        // the Accessibility category on modern macOS, and CGEventTap attempts
        // can bail on Accessibility before they reach the Input Monitoring
        // check). Once the user grants Accessibility and relaunches, this
        // branch is skipped and we register the other grants cleanly below.
        log::info!("firing first-launch Accessibility prompt");
        unsafe {
            let key = kAXTrustedCheckOptionPrompt;
            let value = kCFBooleanTrue;
            let options = CFDictionaryCreate(
                kCFAllocatorDefault,
                &key as *const _,
                &value as *const _,
                1,
                kCFTypeDictionaryKeyCallBacks,
                kCFTypeDictionaryValueCallBacks,
            );
            AXIsProcessTrustedWithOptions(options);
            CFRelease(options);
        }
        return;
    }
    // Accessibility is granted. Attempt Input Monitoring registration
    // unconditionally — even if preflight returns true — so the bundle gets
    // listed in System Settings under its own identity (otherwise launches
    // from a parent process that already has Input Monitoring, e.g. Terminal,
    // inherit the grant but the bundle is never listed for the user to
    // toggle persistently).
    log::info!("ensuring Lan Mouse is listed under Input Monitoring");
    unsafe {
        ensure_listed_in_input_monitoring();
    }
    // Same for Post Event: now that Accessibility is present, this call is
    // safe — it won't surface the generic Accessibility prompt.
    log::info!("ensuring Lan Mouse is listed under Accessibility > Post Event");
    unsafe {
        CGRequestPostEventAccess();
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn resolves_only_expected_bundle_layout() {
        let executable = Path::new("/Applications/Lan Mouse.app/Contents/MacOS/lan-mouse");
        assert_eq!(
            bundle_path(executable).unwrap(),
            Path::new("/Applications/Lan Mouse.app")
        );
        assert!(bundle_path(Path::new("/usr/local/bin/lan-mouse")).is_err());
        assert!(
            bundle_path(Path::new(
                "/Applications/Lan Mouse.app/Resources/MacOS/lan-mouse"
            ))
            .is_err()
        );
        assert!(
            bundle_path(Path::new(
                "/Applications/Lan Mouse/Contents/MacOS/lan-mouse"
            ))
            .is_err()
        );
    }

    #[test]
    fn relaunch_arguments_preserve_bundle_path() {
        let bundle = Path::new("/Applications/Lan $Mouse `테스트`.app");
        let (read_end, _write_end) = UnixStream::pair().unwrap();
        let command = relaunch_command(bundle, read_end);
        assert_eq!(command.get_program(), OsStr::new("/bin/sh"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                OsStr::new("-c"),
                OsStr::new(RELAUNCH_AFTER_EXIT_SCRIPT),
                OsStr::new("lan-mouse-relaunch"),
                OsStr::new("/usr/bin/open"),
                bundle.as_os_str(),
            ]
        );
    }

    #[test]
    fn relaunch_waits_for_exit_signal_past_old_delay() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "lan-mouse relaunch $`테스트`-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();

        let opener = directory.join("open");
        let marker = directory.join("opened");
        fs::write(&opener, "#!/bin/sh\n/usr/bin/touch \"$2\"\n").unwrap();
        fs::set_permissions(&opener, fs::Permissions::from_mode(0o700)).unwrap();

        let (read_end, write_end) = UnixStream::pair().unwrap();
        let mut helper = relaunch_command_with_opener(&opener, &marker, read_end)
            .spawn()
            .unwrap();

        // The old implementation opened after one second even if this process
        // was still alive. Keep its exit signal alive past that boundary.
        thread::sleep(Duration::from_millis(1250));
        assert!(!marker.exists());

        drop(write_end);
        let status = (0..40).find_map(|_| {
            let status = helper.try_wait().unwrap();
            if status.is_none() {
                thread::sleep(Duration::from_millis(25));
            }
            status
        });
        if status.is_none() {
            let _ = helper.kill();
            let _ = helper.wait();
        }
        assert!(status.is_some_and(|status| status.success()));
        assert!(marker.exists());

        fs::remove_dir_all(directory).unwrap();
    }
}
