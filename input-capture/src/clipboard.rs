use arboard::Clipboard;
use input_event::{ClipboardEvent, Event};
use lan_mouse_ipc::AppIdent;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::spawn_blocking;
use tokio::time::interval;

use crate::frontmost_app;
use crate::{CaptureError, CaptureEvent};

/// Shared, mutable suppression list. Service owns the canonical
/// `Arc<Mutex<HashSet<AppIdent>>>` and clones the handle into each
/// freshly-spawned [`ClipboardMonitor`]; mutations from
/// `Add/RemoveSuppressedApp` requests take effect immediately on
/// the next clipboard poll.
pub type SuppressionList = Arc<Mutex<HashSet<AppIdent>>>;

/// Clipboard monitor that watches for clipboard changes
pub struct ClipboardMonitor {
    event_rx: Receiver<CaptureEvent>,
    _event_tx: Sender<CaptureEvent>,
    last_content: Arc<Mutex<Option<String>>>,
    last_change: Arc<Mutex<Option<Instant>>>,
    enabled: Arc<Mutex<bool>>,
}

impl ClipboardMonitor {
    /// Construct without app-source suppression. Equivalent to
    /// `with_suppression(Default::default())` — provided as a
    /// convenience for callers that don't care about suppression
    /// (CLI smoke tests, future per-platform unit tests).
    pub fn new() -> Result<Self, CaptureError> {
        Self::with_suppression(SuppressionList::default())
    }

    /// Construct a monitor that consults `suppression` on every
    /// detected clipboard change and skips both the emit AND the
    /// `last_content` update when [`frontmost_app::frontmost_app()`]
    /// reports an app whose [`AppIdent`] is in the list. Skipping
    /// the `last_content` update is intentional: it keeps the
    /// monitor "blind" to the suppressed content so a later non-
    /// suppressed copy of the same string still emits normally.
    pub fn with_suppression(suppression: SuppressionList) -> Result<Self, CaptureError> {
        let (event_tx, event_rx) = mpsc::channel(16);
        let last_content: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let last_change: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let enabled = Arc::new(Mutex::new(true));

        let last_content_clone = last_content.clone();
        let last_change_clone = last_change.clone();
        let enabled_clone = enabled.clone();
        let event_tx_clone = event_tx.clone();
        let suppression_clone = suppression.clone();

        // Spawn monitoring task. Cadence: 100 ms on macOS (cheap
        // because `pasteboard_change_count_advanced` short-circuits
        // 99% of ticks via a single integer compare); 500 ms
        // elsewhere (no cheap precheck, full content read every
        // tick). Tighter cadence on macOS shrinks the
        // frontmost-app suppression race window from 500 ms →
        // 100 ms — the user would have to Cmd+Tab faster than
        // human reaction time after copying to defeat the check.
        #[cfg(target_os = "macos")]
        const POLL_MS: u64 = 100;
        #[cfg(not(target_os = "macos"))]
        const POLL_MS: u64 = 500;

        tokio::spawn(async move {
            let mut check_interval = interval(Duration::from_millis(POLL_MS));

            loop {
                check_interval.tick().await;

                // Check if enabled
                let is_enabled = {
                    let enabled = enabled_clone.lock().unwrap();
                    *enabled
                };

                if !is_enabled {
                    continue;
                }

                // macOS: skip the expensive content-read entirely
                // when NSPasteboard.changeCount hasn't advanced
                // since last tick. This is the canonical clipboard-
                // monitor optimization (Maccy / Alfred / Paste all
                // do it). Single integer compare per idle tick.
                #[cfg(target_os = "macos")]
                if !pasteboard_change_count_advanced() {
                    continue;
                }

                // Read clipboard in blocking task
                let last_content_clone2 = last_content_clone.clone();
                let last_change_clone2 = last_change_clone.clone();
                let event_tx_clone2 = event_tx_clone.clone();
                let suppression_clone2 = suppression_clone.clone();

                let _ = spawn_blocking(move || {
                    // Create clipboard instance
                    let mut clipboard = match Clipboard::new() {
                        Ok(c) => c,
                        Err(e) => {
                            log::debug!("Failed to create clipboard: {}", e);
                            return;
                        }
                    };

                    // Get current clipboard text
                    let current_text = match clipboard.get_text() {
                        Ok(text) => {
                            log::trace!("Clipboard text read: {} bytes", text.len());
                            text
                        }
                        Err(e) => {
                            // Clipboard might be empty or contain non-text data
                            log::trace!("Failed to get clipboard text: {}", e);
                            return;
                        }
                    };

                    // Check if content changed
                    let mut last_content = last_content_clone2.lock().unwrap();
                    let mut last_change = last_change_clone2.lock().unwrap();

                    let last_change_elapsed = last_change.map(|t| t.elapsed());
                    // App-source suppression. Frontmost-app lookup
                    // and the macOS concealed-pasteboard probe both
                    // run only after we've decided the content
                    // actually changed (see PollDecision::classify).
                    // Pre-compute the inputs here so the classifier
                    // stays a pure function.
                    let needs_decision = PollDecision::content_might_emit(
                        &current_text,
                        last_content.as_deref(),
                        last_change_elapsed,
                    );
                    let (concealed, suppressed) = if needs_decision {
                        let concealed = is_concealed_clipboard();
                        let suppressed = if concealed {
                            None
                        } else {
                            is_suppressed(&suppression_clone2)
                        };
                        (concealed, suppressed)
                    } else {
                        (false, None)
                    };
                    let decision = PollDecision::classify(
                        &current_text,
                        last_content.as_deref(),
                        last_change_elapsed,
                        concealed,
                        suppressed.is_some(),
                    );
                    // Always advance `last_content` after a state-
                    // changing decision (Suppressed or Emit),
                    // regardless of which suppression branch
                    // fired. The earlier "blind to suppressed
                    // value" approach left `last_content` at the
                    // previous emitted value, which made every
                    // subsequent 500ms poll see the SAME
                    // suppressed content as "changed" and re-run
                    // the suppression check. Any focus shift
                    // between polls (user alt-tabs after copying
                    // a password) would then find a non-
                    // suppressed frontmost and leak the password.
                    // PollDecision::advances_state pins this
                    // contract — see the unit tests at the
                    // bottom of this file.
                    if decision.advances_state() {
                        *last_content = Some(current_text.clone());
                        *last_change = Some(Instant::now());
                    }
                    match decision {
                        PollDecision::Unchanged => {}
                        PollDecision::Debounced => {
                            log::trace!("Clipboard changed but debounced (too recent)");
                        }
                        PollDecision::Suppressed if concealed => {
                            log::debug!(
                                "clipboard change suppressed (concealed pasteboard type)"
                            );
                        }
                        PollDecision::Suppressed => {
                            if let Some(app) = suppressed.as_ref() {
                                log::debug!(
                                    "clipboard change suppressed (frontmost app `{}`)",
                                    app.label()
                                );
                            }
                        }
                        PollDecision::Emit => {
                            log::info!(
                                "Clipboard changed, length: {} bytes",
                                current_text.len()
                            );
                            let event = CaptureEvent::Input(Event::Clipboard(
                                ClipboardEvent::Text(current_text),
                            ));
                            let _ = event_tx_clone2.blocking_send(event);
                        }
                    }
                })
                .await;
            }
        });

        Ok(Self {
            event_rx,
            _event_tx: event_tx,
            last_content,
            last_change,
            enabled,
        })
    }

    /// Receive the next clipboard event
    pub async fn recv(&mut self) -> Option<CaptureEvent> {
        self.event_rx.recv().await
    }

    /// Enable clipboard monitoring
    pub fn enable(&self) {
        let mut enabled = self.enabled.lock().unwrap();
        *enabled = true;
        log::info!("Clipboard monitoring enabled");
    }

    /// Disable clipboard monitoring
    pub fn disable(&self) {
        let mut enabled = self.enabled.lock().unwrap();
        *enabled = false;
        log::info!("Clipboard monitoring disabled");
    }

    /// Update the last known clipboard content (called when we set the clipboard)
    /// This prevents detecting our own clipboard changes as external changes
    pub fn update_last_content(&self, content: String) {
        let mut last_content = self.last_content.lock().unwrap();
        let mut last_change = self.last_change.lock().unwrap();
        *last_content = Some(content);
        *last_change = Some(Instant::now());
    }
}

/// macOS password managers stamp `org.nspasteboard.ConcealedType`
/// on the general pasteboard so cooperating apps skip syncing
/// passwords. Returns `true` when that UTI is present on the
/// current pasteboard contents. Always `false` on non-macOS.
///
/// This is the standard "nspasteboard.com" convention — see
/// <https://nspasteboard.org/>.
#[cfg(target_os = "macos")]
fn is_concealed_clipboard() -> bool {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;

    let pasteboard = NSPasteboard::generalPasteboard();
    let Some(types) = pasteboard.types() else {
        return false;
    };
    let concealed = NSString::from_str("org.nspasteboard.ConcealedType");
    types.iter().any(|t| t.isEqualToString(&concealed))
}

#[cfg(not(target_os = "macos"))]
fn is_concealed_clipboard() -> bool {
    false
}

/// If [`frontmost_app::frontmost_app()`] reports an app whose ident
/// is in the suppression list, return that ident. Otherwise return
/// `None`. Snapshotting the lock guard short keeps us from holding
/// the mutex across the platform call (which on Linux can shell
/// out to hyprctl/swaymsg).
fn is_suppressed(list: &SuppressionList) -> Option<AppIdent> {
    let snapshot: Vec<AppIdent> = {
        let Ok(guard) = list.lock() else {
            log::debug!("clipboard suppression: lock poisoned");
            return None;
        };
        if guard.is_empty() {
            log::debug!("clipboard suppression: list is empty");
            return None;
        }
        guard.iter().cloned().collect()
    };
    let active = frontmost_app::frontmost_app();
    log::debug!(
        "clipboard suppression check: list={:?} active={:?}",
        snapshot,
        active
    );
    let active = active?;
    snapshot.into_iter().find(|s| s.matches(&active))
}

/// Returns `true` the first time it's called, and on every later
/// call where `NSPasteboard.generalPasteboard.changeCount` has
/// advanced since the previous call. Used as a cheap precheck so
/// the polling loop only invokes `arboard::Clipboard::get_text`
/// (which round-trips through `pboardd` via XPC) on ticks where
/// the pasteboard actually mutated.
///
/// `changeCount` reads are an Apple-blessed background-thread
/// operation — the property is designed for exactly this kind of
/// polling. No autorelease pool needed: the return value is a
/// primitive `NSInteger`, not an Objective-C object.
#[cfg(target_os = "macos")]
fn pasteboard_change_count_advanced() -> bool {
    use objc2_app_kit::NSPasteboard;
    use std::sync::atomic::{AtomicI64, Ordering};

    // Initial sentinel `i64::MIN` ensures the first call always
    // returns `true` so we read once at startup to seed the
    // diff-against-`last_content` machinery downstream.
    static LAST: AtomicI64 = AtomicI64::new(i64::MIN);

    let pb = NSPasteboard::generalPasteboard();
    let now = pb.changeCount() as i64;
    let prev = LAST.swap(now, Ordering::Relaxed);
    prev != now
}

/// Outcome of a single clipboard-poll cycle. Pulled into a pure
/// function so the focus-race invariant — "advance `last_content`
/// on every state-changing decision, even when we suppress" — is
/// pinned by unit tests rather than implicit in the closure
/// body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollDecision {
    /// Current text equals last recorded — nothing to do.
    Unchanged,
    /// Content changed but the 200 ms debounce window from the
    /// previous advance hasn't elapsed yet.
    Debounced,
    /// Content changed, debounce passed, but suppression
    /// (concealed pasteboard or app-list match) tells us not to
    /// emit. State must still advance so subsequent polls don't
    /// re-check the same content and leak on a focus shift.
    Suppressed,
    /// Content changed, debounce passed, suppression cleared.
    /// Caller emits the event and advances state.
    Emit,
}

impl PollDecision {
    /// Cheap pre-flight: `true` when the next call to
    /// [`PollDecision::classify`] could return `Suppressed` or
    /// `Emit`. The poll loop uses this to skip the relatively
    /// expensive `is_concealed_clipboard()` / `frontmost_app()` /
    /// `is_suppressed()` calls when the content hasn't changed
    /// or the debounce window blocks emission anyway.
    fn content_might_emit(
        current_text: &str,
        last_content: Option<&str>,
        last_change_elapsed: Option<Duration>,
    ) -> bool {
        if last_content == Some(current_text) {
            return false;
        }
        last_change_elapsed.is_none_or(|d| d > Duration::from_millis(200))
    }

    /// Decide what to do with this poll cycle. Pure function — no
    /// I/O, no global state — so the focus-race invariant is
    /// expressible as a series of `assert_eq!` calls.
    fn classify(
        current_text: &str,
        last_content: Option<&str>,
        last_change_elapsed: Option<Duration>,
        concealed: bool,
        suppressed_match: bool,
    ) -> Self {
        if last_content == Some(current_text) {
            return Self::Unchanged;
        }
        let debounce_passed =
            last_change_elapsed.is_none_or(|d| d > Duration::from_millis(200));
        if !debounce_passed {
            return Self::Debounced;
        }
        if concealed || suppressed_match {
            Self::Suppressed
        } else {
            Self::Emit
        }
    }

    /// True when this decision must advance `last_content` /
    /// `last_change` in the caller. Both `Suppressed` and `Emit`
    /// are state-changing — the focus-race fix lives here.
    fn advances_state(self) -> bool {
        matches!(self, Self::Suppressed | Self::Emit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_unchanged_when_text_matches_last_content() {
        let d = PollDecision::classify(
            "secret",
            Some("secret"),
            Some(Duration::from_secs(60)),
            false,
            false,
        );
        assert_eq!(d, PollDecision::Unchanged);
        assert!(!d.advances_state());
    }

    #[test]
    fn classify_debounced_when_recent_change() {
        // Content differs but only 100ms since last advance —
        // hold off so a peer's set_clipboard echo doesn't bounce
        // back as a fresh local emit.
        let d = PollDecision::classify(
            "new",
            Some("old"),
            Some(Duration::from_millis(100)),
            false,
            false,
        );
        assert_eq!(d, PollDecision::Debounced);
        assert!(!d.advances_state());
    }

    #[test]
    fn classify_emit_when_changed_unsuppressed() {
        let d = PollDecision::classify(
            "new",
            Some("old"),
            Some(Duration::from_secs(1)),
            false,
            false,
        );
        assert_eq!(d, PollDecision::Emit);
        assert!(d.advances_state());
    }

    #[test]
    fn classify_emit_on_first_change() {
        // No prior advance: the first poll always falls through
        // to Emit (assuming nothing's suppressed) so the very
        // first clipboard read after startup is broadcast.
        let d = PollDecision::classify("first", None, None, false, false);
        assert_eq!(d, PollDecision::Emit);
    }

    #[test]
    fn classify_suppressed_when_concealed() {
        let d = PollDecision::classify(
            "password",
            Some("plain"),
            Some(Duration::from_secs(1)),
            true,
            false,
        );
        assert_eq!(d, PollDecision::Suppressed);
    }

    #[test]
    fn classify_suppressed_when_app_list_matches() {
        let d = PollDecision::classify(
            "password",
            Some("plain"),
            Some(Duration::from_secs(1)),
            false,
            true,
        );
        assert_eq!(d, PollDecision::Suppressed);
    }

    /// The focus-race invariant: a Suppressed decision MUST tell
    /// the caller to advance `last_content`. If this assert
    /// fails, the regression we hit live (1Password password
    /// leaks on Ghostty alt-tab after copy) is back.
    #[test]
    fn suppressed_decision_advances_state() {
        let d = PollDecision::classify(
            "password",
            Some("plain"),
            Some(Duration::from_secs(1)),
            false,
            true,
        );
        assert!(
            d.advances_state(),
            "Suppressed must advance last_content; otherwise the \
             same content gets re-checked on the next poll and a \
             focus shift between polls will leak the password."
        );
    }

    /// Companion: Unchanged and Debounced must NOT advance state.
    /// Advancing on Debounced would defeat the 200 ms window and
    /// turn every peer-driven clipboard sync into a fresh local
    /// emit (echo loop).
    #[test]
    fn non_state_changing_decisions_do_not_advance() {
        for (current, last, elapsed) in [
            ("same", Some("same"), Some(Duration::from_secs(1))),
            ("new", Some("old"), Some(Duration::from_millis(50))),
        ] {
            let d = PollDecision::classify(current, last, elapsed, false, false);
            assert!(
                !d.advances_state(),
                "{d:?} should not advance state (current={current:?} last={last:?} elapsed={elapsed:?})"
            );
        }
    }

    #[test]
    fn content_might_emit_skips_unchanged_and_debounced() {
        // Used by the poll loop to short-circuit the expensive
        // suppression probes. Same matrix as `classify`'s "no
        // emission possible" cases.
        assert!(!PollDecision::content_might_emit(
            "same",
            Some("same"),
            None
        ));
        assert!(!PollDecision::content_might_emit(
            "new",
            Some("old"),
            Some(Duration::from_millis(50))
        ));
        assert!(PollDecision::content_might_emit(
            "new",
            Some("old"),
            Some(Duration::from_secs(1))
        ));
        assert!(PollDecision::content_might_emit("new", None, None));
    }
}
