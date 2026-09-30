//! Temporary diagnostics for lost pointer motion after a lock / unlock.
//!
//! Logs one `[diag] hook:` line per second (debug level) with statistics
//! about the low-level mouse hook, so the hook side can be compared with
//! the `[diag] service:` lines of the main task.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

const REPORT_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) enum Outcome {
    /// motion was sent to the main task
    Sent,
    /// channel was full, motion was deferred
    Deferred,
    /// no client active, event passed to the system
    Passed,
}

struct HookStats {
    start: Instant,
    last_call: Option<Instant>,
    last_os_time: Option<u32>,
    calls: u32,
    sent: u32,
    deferred: u32,
    passed: u32,
    /// max time between two hook calls, as seen by the hook thread
    max_gap: Duration,
    /// max time between two events, according to the event timestamps
    max_os_gap: u32,
    /// time between the event timestamp and the hook call
    total_lag: u64,
    max_lag: u32,
}

impl HookStats {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            last_call: None,
            last_os_time: None,
            calls: 0,
            sent: 0,
            deferred: 0,
            passed: 0,
            max_gap: Duration::ZERO,
            max_os_gap: 0,
            total_lag: 0,
            max_lag: 0,
        }
    }
}

thread_local! {
    static STATS: RefCell<Option<HookStats>> = const { RefCell::new(None) };
}

/// Records one `WM_MOUSEMOVE` hook call.
///
/// `os_time` is the `time` field of the `MSLLHOOKSTRUCT`, `entry_point` is
/// where the cursor is supposed to be parked while a client is active.
pub(crate) fn record_motion(os_time: u32, outcome: Outcome, entry_point: Option<(i32, i32)>) {
    STATS.with_borrow_mut(|stats| {
        let stats = stats.get_or_insert_with(HookStats::new);
        let now = Instant::now();

        stats.calls += 1;
        match outcome {
            Outcome::Sent => stats.sent += 1,
            Outcome::Deferred => stats.deferred += 1,
            Outcome::Passed => stats.passed += 1,
        }
        if let Some(last) = stats.last_call {
            stats.max_gap = stats.max_gap.max(now - last);
        }
        if let Some(last) = stats.last_os_time {
            stats.max_os_gap = stats.max_os_gap.max(os_time.wrapping_sub(last));
        }
        let lag = unsafe { GetTickCount() }.wrapping_sub(os_time);
        stats.total_lag += lag as u64;
        stats.max_lag = stats.max_lag.max(lag);
        stats.last_call = Some(now);
        stats.last_os_time = Some(os_time);

        if now - stats.start < REPORT_INTERVAL {
            return;
        }

        /* offset of the real cursor from where it should be parked */
        let cursor_offset = entry_point.map(|(ex, ey)| {
            let mut p = POINT::default();
            let _ = unsafe { GetCursorPos(&mut p) };
            (p.x - ex, p.y - ey)
        });

        log::debug!(
            "[diag] hook: calls={} sent={} deferred={} passed={} max_gap={}ms max_os_gap={}ms lag_avg={}ms lag_max={}ms cursor_offset={:?}",
            stats.calls,
            stats.sent,
            stats.deferred,
            stats.passed,
            stats.max_gap.as_millis(),
            stats.max_os_gap,
            stats.total_lag / stats.calls as u64,
            stats.max_lag,
            cursor_offset,
        );

        let (last_call, last_os_time) = (stats.last_call, stats.last_os_time);
        *stats = HookStats::new();
        stats.last_call = last_call;
        stats.last_os_time = last_os_time;
    });
}
