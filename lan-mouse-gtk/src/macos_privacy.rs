#![cfg(target_os = "macos")]

//! Tiny macOS Privacy-pane helpers used by the GUI.
//!
//! On macOS 13+, the Accessibility grant transitively confers the
//! listen-only event-tap privilege that Input Monitoring gates and the
//! synthesize-event privilege that Post Event gates, and the bundle
//! typically isn't even listed in those separate panes. So the single
//! user-facing action for any missing-capture or missing-emulation
//! scenario is "re-toggle Accessibility" — we don't route elsewhere.

use std::ffi::{c_uchar, c_void};
use std::process::Command;
use std::sync::Once;

use gtk::glib;

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


/// Poll for an Accessibility grant transition. Starts a 1-second GLib
/// timer that fires `on_granted` once, the first time
/// `AXIsProcessTrusted()` returns true. A no-op if AX is already granted.
///
/// We rely on polling rather than AXObserver because the AX notification
/// API requires a trusted process to subscribe — the precondition we're
/// waiting for. This runs on the GTK main thread (via timeout_add_seconds_local).
pub fn watch_for_accessibility_grant<F>(mut on_granted: F)
where
    F: FnMut() + 'static,
{
    if accessibility_granted() {
        return;
    }
    log::info!("watching for Accessibility grant");
    glib::timeout_add_seconds_local(1, move || {
        if accessibility_granted() {
            log::info!("Accessibility granted; firing relaunch prompt");
            on_granted();
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

pub fn open_accessibility_settings() {
    open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
}

/// Spawn a fresh instance of the current `.app` bundle via Launch Services
/// after a 1-second delay, so the new instance starts *after* the current
/// process has exited — otherwise Launch Services reactivates the existing
/// process instead of launching a fresh one, and the stale IPC socket
/// would block the new daemon subprocess. The caller is responsible for
/// quitting the current process (e.g. `Application::quit()`) after this.
pub fn relaunch_bundle() {
    // Resolve the .app bundle path from the current executable: it lives
    // at <bundle>/Contents/MacOS/lan-mouse, so three parents up is the
    // bundle root we hand to `open`.
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(bundle) = exe
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
    else {
        return;
    };

    // Trailing `&` backgrounds the sleep+open so our shell call returns
    // immediately; the spawned shell is adopted by launchd once we exit.
    let cmd = format!("(sleep 1 && open {bundle:?}) &");
    let _ = Command::new("sh").arg("-c").arg(cmd).spawn();
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
