use arboard::Clipboard;
use input_event::ClipboardEvent;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::task::spawn_blocking;

#[derive(Debug, Error)]
pub enum ClipboardError {
    #[error("Failed to access clipboard: {0}")]
    Access(String),
    #[error("Failed to set clipboard: {0}")]
    Set(String),
}

/// Clipboard emulation that sets clipboard content
#[derive(Clone)]
pub struct ClipboardEmulation {
    // Use Arc<Mutex<>> to share clipboard across threads
    clipboard: Arc<Mutex<Option<Clipboard>>>,
}

impl ClipboardEmulation {
    pub fn new() -> Result<Self, ClipboardError> {
        // Try to create initial clipboard instance
        let clipboard = match Clipboard::new() {
            Ok(c) => Some(c),
            Err(e) => {
                log::warn!("Failed to create clipboard instance: {}", e);
                None
            }
        };

        Ok(Self {
            clipboard: Arc::new(Mutex::new(clipboard)),
        })
    }

    /// Set clipboard content from a clipboard event
    pub async fn set(&self, event: ClipboardEvent) -> Result<(), ClipboardError> {
        match event {
            ClipboardEvent::Text(text) => {
                let clipboard_arc = self.clipboard.clone();

                spawn_blocking(move || {
                    let mut clipboard_guard = clipboard_arc.lock().unwrap();

                    // Try to get or create clipboard
                    let clipboard = match clipboard_guard.as_mut() {
                        Some(c) => c,
                        None => {
                            // Try to create a new clipboard instance
                            match Clipboard::new() {
                                Ok(c) => {
                                    *clipboard_guard = Some(c);
                                    clipboard_guard.as_mut().unwrap()
                                }
                                Err(e) => {
                                    return Err(ClipboardError::Access(format!("{}", e)));
                                }
                            }
                        }
                    };

                    // Set clipboard text
                    clipboard
                        .set_text(text.clone())
                        .map_err(|e| ClipboardError::Set(format!("{}", e)))?;

                    log::debug!("Clipboard set, length: {} bytes", text.len());
                    Ok(())
                })
                .await
                .map_err(|e| ClipboardError::Access(format!("Task join error: {}", e)))?
            }
        }
    }

    /// Get current clipboard content (for testing/verification)
    pub async fn get(&self) -> Result<String, ClipboardError> {
        let clipboard_arc = self.clipboard.clone();

        spawn_blocking(move || {
            let mut clipboard_guard = clipboard_arc.lock().unwrap();

            let clipboard = match clipboard_guard.as_mut() {
                Some(c) => c,
                None => match Clipboard::new() {
                    Ok(c) => {
                        *clipboard_guard = Some(c);
                        clipboard_guard.as_mut().unwrap()
                    }
                    Err(e) => {
                        return Err(ClipboardError::Access(format!("{}", e)));
                    }
                },
            };

            clipboard
                .get_text()
                .map_err(|e| ClipboardError::Access(format!("{}", e)))
        })
        .await
        .map_err(|e| ClipboardError::Access(format!("Task join error: {}", e)))?
    }
}
