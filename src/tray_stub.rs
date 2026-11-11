//! Builds without a tray icon (no `tray` feature, or not Linux / BSD): the
//! same interface as `tray.rs`, but nothing is shown and no request ever
//! arrives.

/// Requests from the tray menu to the service.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum TrayCommand {
    /// start (`true`) or stop (`false`) sharing
    SetSharing(bool),
    /// shut the service down
    Quit,
}

#[derive(Default)]
pub(crate) struct ServiceTray;

impl ServiceTray {
    pub(crate) async fn new(_sharing: bool) -> Self {
        Self
    }

    pub(crate) async fn command(&mut self) -> TrayCommand {
        std::future::pending().await
    }

    pub(crate) fn invalidate(&mut self) {}

    pub(crate) async fn set_sharing(&mut self, _sharing: bool) {}

    pub(crate) async fn shutdown(&mut self) {}
}
