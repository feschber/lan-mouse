//! Temporary diagnostics for lost pointer motion after a lock / unlock.
//!
//! Logs one `[diag] service:` line per second (debug level) with statistics
//! about how fast the capture loop consumes and forwards captured events.

use std::cell::RefCell;
use std::time::{Duration, Instant};

const REPORT_INTERVAL: Duration = Duration::from_secs(1);

struct ServiceStats {
    start: Instant,
    last_done: Option<Instant>,
    events: u32,
    /// time spent handling (i.e. mostly sending) events
    total_handle: Duration,
    max_handle: Duration,
    /// max time between finishing one event and starting the next,
    /// i.e. waiting for input or running other tasks on the runtime
    max_gap: Duration,
}

impl ServiceStats {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            last_done: None,
            events: 0,
            total_handle: Duration::ZERO,
            max_handle: Duration::ZERO,
            max_gap: Duration::ZERO,
        }
    }
}

thread_local! {
    static STATS: RefCell<Option<ServiceStats>> = const { RefCell::new(None) };
}

/// Records one handled capture event that started at `started`.
pub(crate) fn record_capture_event(started: Instant) {
    STATS.with_borrow_mut(|stats| {
        let stats = stats.get_or_insert_with(ServiceStats::new);
        let now = Instant::now();
        let handle = now - started;

        stats.events += 1;
        stats.total_handle += handle;
        stats.max_handle = stats.max_handle.max(handle);
        if let Some(last) = stats.last_done {
            stats.max_gap = stats.max_gap.max(started.saturating_duration_since(last));
        }
        stats.last_done = Some(now);

        if now - stats.start < REPORT_INTERVAL {
            return;
        }

        log::debug!(
            "[diag] service: events={} handle_avg={}us handle_max={}us max_gap={}ms",
            stats.events,
            (stats.total_handle / stats.events).as_micros(),
            stats.max_handle.as_micros(),
            stats.max_gap.as_millis(),
        );

        let last_done = stats.last_done;
        *stats = ServiceStats::new();
        stats.last_done = last_done;
    });
}
