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

        // Spawn monitoring task
        tokio::spawn(async move {
            let mut check_interval = interval(Duration::from_millis(500));

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

                    let content_changed = match last_content.as_ref() {
                        None => true,
                        Some(last) => last != &current_text,
                    };

                    if content_changed {
                        // Debounce: ignore changes within 200ms of last change
                        // This prevents infinite loops when both sides update clipboard
                        let should_emit = match *last_change {
                            None => true,
                            Some(instant) => instant.elapsed() > Duration::from_millis(200),
                        };

                        if should_emit {
                            // App-source suppression. Frontmost-app
                            // lookup happens here (not on every
                            // poll) so we only pay the cost when
                            // the clipboard actually changed.
                            let suppressed = is_suppressed(&suppression_clone2);
                            if let Some(app) = suppressed {
                                log::debug!(
                                    "clipboard change suppressed (frontmost app `{}`)",
                                    app.label()
                                );
                                // Intentionally NOT updating
                                // last_content / last_change. If
                                // the user later copies a non-
                                // suppressed value followed by the
                                // same suppressed text, we still
                                // want the non-suppressed copy to
                                // emit and the suppressed re-copy
                                // to be re-evaluated (and re-
                                // suppressed) — keeping
                                // last_content "blind" to the
                                // suppressed value preserves that.
                            } else {
                                log::info!(
                                    "Clipboard changed, length: {} bytes",
                                    current_text.len()
                                );
                                *last_content = Some(current_text.clone());
                                *last_change = Some(Instant::now());

                                // Send event
                                let event = CaptureEvent::Input(Event::Clipboard(
                                    ClipboardEvent::Text(current_text),
                                ));
                                let _ = event_tx_clone2.blocking_send(event);
                            }
                        } else {
                            log::trace!("Clipboard changed but debounced (too recent)");
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

/// If [`frontmost_app::frontmost_app()`] reports an app whose ident
/// is in the suppression list, return that ident. Otherwise return
/// `None`. Snapshotting the lock guard short keeps us from holding
/// the mutex across the platform call (which on Linux can shell
/// out to hyprctl/swaymsg).
fn is_suppressed(list: &SuppressionList) -> Option<AppIdent> {
    let snapshot: Vec<AppIdent> = {
        let guard = list.lock().ok()?;
        if guard.is_empty() {
            return None;
        }
        guard.iter().cloned().collect()
    };
    let active = frontmost_app::frontmost_app()?;
    snapshot.into_iter().find(|s| s.matches(&active))
}
