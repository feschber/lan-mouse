mod imp;

use std::collections::HashMap;

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::{Object, clone};
use gtk::{
    NoSelection, gio,
    glib::{self, closure_local},
};

use lan_mouse_ipc::{
    ClientConfig, ClientHandle, ClientState, DEFAULT_PORT, FrontendRequest, FrontendRequestWriter,
    Position,
};

use crate::{
    authorization_window::AuthorizationWindow, clipboard_privacy_window::ClipboardPrivacyWindow,
    fingerprint_window::FingerprintWindow, key_object::KeyObject, key_row::KeyRow,
};

use super::{client_object::ClientObject, client_row::ClientRow};

#[cfg(target_os = "macos")]
fn set_button_content_label(button: &gtk::Button, label: &str) {
    // The Reenable/Grant/Relaunch button wraps its icon+label in an
    // AdwButtonContent (see window.ui). Walk into it and swap the label
    // rather than GtkButton::set_label, which would replace the content
    // widget and drop the icon.
    if let Some(content) = button.child().and_downcast::<adw::ButtonContent>() {
        content.set_label(label);
    }
}

glib::wrapper! {
    pub struct Window(ObjectSubclass<imp::Window>)
        @extends adw::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl Window {
    pub(super) fn new(app: &adw::Application, conn: FrontendRequestWriter) -> Self {
        let window: Self = Object::builder().property("application", app).build();
        window
            .imp()
            .frontend_request_writer
            .borrow_mut()
            .replace(conn);
        window
    }

    fn clients(&self) -> gio::ListStore {
        self.imp()
            .clients
            .borrow()
            .clone()
            .expect("Could not get clients")
    }

    fn authorized(&self) -> gio::ListStore {
        self.imp()
            .authorized
            .borrow()
            .clone()
            .expect("Could not get authorized")
    }

    fn client_by_idx(&self, idx: u32) -> Option<ClientObject> {
        self.clients().item(idx).map(|o| o.downcast().unwrap())
    }

    fn authorized_by_idx(&self, idx: u32) -> Option<KeyObject> {
        self.authorized().item(idx).map(|o| o.downcast().unwrap())
    }

    fn row_by_idx(&self, idx: i32) -> Option<ClientRow> {
        self.imp()
            .client_list
            .get()
            .row_at_index(idx)
            .map(|o| o.downcast().expect("expected ClientRow"))
    }

    fn setup_authorized(&self) {
        let store = gio::ListStore::new::<KeyObject>();
        self.imp().authorized.replace(Some(store));
        let selection_model = NoSelection::new(Some(self.authorized()));
        self.imp().authorized_list.bind_model(
            Some(&selection_model),
            clone!(
                #[weak(rename_to = window)]
                self,
                #[upgrade_or_panic]
                move |obj| {
                    let key_obj = obj.downcast_ref().expect("object of type `KeyObject`");
                    let row = window.create_key_row(key_obj);
                    row.connect_closure(
                        "request-delete",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: KeyRow| {
                                if let Some(key_obj) = window.authorized_by_idx(row.index() as u32)
                                {
                                    window.request_fingerprint_remove(key_obj.get_fingerprint());
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-natural-scroll-change",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: KeyRow, natural_scroll: bool| {
                                if let Some(key_obj) = window.authorized_by_idx(row.index() as u32)
                                {
                                    window.request(FrontendRequest::SetIncomingPeerNaturalScroll(
                                        key_obj.get_fingerprint(),
                                        natural_scroll,
                                    ));
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-sensitivity-change",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: KeyRow, sensitivity: f64| {
                                if let Some(key_obj) = window.authorized_by_idx(row.index() as u32)
                                {
                                    window.request(FrontendRequest::SetIncomingPeerSensitivity(
                                        key_obj.get_fingerprint(),
                                        sensitivity,
                                    ));
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-clipboard-receive-change",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: KeyRow, clipboard_receive: bool| {
                                if let Some(key_obj) = window.authorized_by_idx(row.index() as u32)
                                {
                                    window.request(
                                        FrontendRequest::SetIncomingPeerClipboardReceive(
                                            key_obj.get_fingerprint(),
                                            clipboard_receive,
                                        ),
                                    );
                                }
                            }
                        ),
                    );
                    row.upcast()
                }
            ),
        )
    }

    fn setup_clients(&self) {
        let model = gio::ListStore::new::<ClientObject>();
        self.imp().clients.replace(Some(model));

        let selection_model = NoSelection::new(Some(self.clients()));
        self.imp().client_list.bind_model(
            Some(&selection_model),
            clone!(
                #[weak(rename_to = window)]
                self,
                #[upgrade_or_panic]
                move |obj| {
                    let client_object = obj
                        .downcast_ref()
                        .expect("Expected object of type `ClientObject`.");
                    let row = window.create_client_row(client_object);
                    row.connect_closure(
                        "request-hostname-change",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: ClientRow, hostname: String| {
                                log::debug!("request-hostname-change");
                                if let Some(client) = window.client_by_idx(row.index() as u32) {
                                    let hostname = Some(hostname).filter(|s| !s.is_empty());
                                    /* changed in response to FrontendEvent
                                     * -> do not request additional update */
                                    window.request(FrontendRequest::UpdateHostname(
                                        client.handle(),
                                        hostname,
                                    ));
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-port-change",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: ClientRow, port: u32| {
                                if let Some(client) = window.client_by_idx(row.index() as u32) {
                                    window.request(FrontendRequest::UpdatePort(
                                        client.handle(),
                                        port as u16,
                                    ));
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-activate",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: ClientRow, active: bool| {
                                if let Some(client) = window.client_by_idx(row.index() as u32) {
                                    log::debug!(
                                        "request: {} client",
                                        if active { "activating" } else { "deactivating" }
                                    );
                                    window.request(FrontendRequest::Activate(
                                        client.handle(),
                                        active,
                                    ));
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-delete",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: ClientRow| {
                                if let Some(client) = window.client_by_idx(row.index() as u32) {
                                    window.request(FrontendRequest::Delete(client.handle()));
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-dns",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: ClientRow| {
                                if let Some(client) = window.client_by_idx(row.index() as u32) {
                                    window.request(FrontendRequest::ResolveDns(
                                        client.get_data().handle,
                                    ));
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-position-change",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: ClientRow, pos_idx: u32| {
                                if let Some(client) = window.client_by_idx(row.index() as u32) {
                                    let position = match pos_idx {
                                        0 => Position::Left,
                                        1 => Position::Right,
                                        2 => Position::Top,
                                        _ => Position::Bottom,
                                    };
                                    window.request(FrontendRequest::UpdatePosition(
                                        client.handle(),
                                        position,
                                    ));
                                }
                            }
                        ),
                    );
                    row.connect_closure(
                        "request-clipboard-send-change",
                        false,
                        closure_local!(
                            #[strong]
                            window,
                            move |row: ClientRow, clipboard_send: bool| {
                                if let Some(client) = window.client_by_idx(row.index() as u32) {
                                    window.request(FrontendRequest::SetClientClipboardSend(
                                        client.handle(),
                                        clipboard_send,
                                    ));
                                }
                            }
                        ),
                    );
                    row.upcast()
                }
            ),
        );
    }

    fn setup_icon(&self) {
        self.set_icon_name(Some("de.feschber.LanMouse"));
    }

    /// workaround for a bug in libadwaita that shows an ugly line beneath
    /// the last element if a placeholder is set.
    /// https://gitlab.gnome.org/GNOME/gtk/-/merge_requests/6308
    fn update_placeholder_visibility(&self) {
        let visible = self.clients().n_items() == 0;
        let placeholder = self.imp().client_placeholder.get();
        self.imp().client_list.set_placeholder(match visible {
            true => Some(&placeholder),
            false => None,
        });
    }

    fn update_auth_placeholder_visibility(&self) {
        let visible = self.authorized().n_items() == 0;
        let placeholder = self.imp().authorized_placeholder.get();
        self.imp().authorized_list.set_placeholder(match visible {
            true => Some(&placeholder),
            false => None,
        });
    }

    fn create_client_row(&self, client_object: &ClientObject) -> ClientRow {
        let row = ClientRow::new(client_object);
        row.bind(client_object);
        row
    }

    fn create_key_row(&self, key_object: &KeyObject) -> KeyRow {
        let row = KeyRow::new();
        row.bind(key_object);
        row
    }

    pub(super) fn new_client(
        &self,
        handle: ClientHandle,
        client: ClientConfig,
        state: ClientState,
    ) {
        let client = ClientObject::new(handle, client, state.clone());
        self.clients().append(&client);
        self.update_placeholder_visibility();
        self.update_dns_state(handle, !state.ips.is_empty());
    }

    pub(super) fn update_client_list(
        &self,
        clients: Vec<(ClientHandle, ClientConfig, ClientState)>,
    ) {
        for (handle, client, state) in clients {
            if self.client_idx(handle).is_some() {
                self.update_client_config(handle, client);
                self.update_client_state(handle, state);
            } else {
                self.new_client(handle, client, state);
            }
        }
    }

    pub(super) fn update_port(&self, port: u16, msg: Option<String>) {
        if let Some(msg) = msg {
            self.show_toast(msg.as_str());
        }
        self.imp().set_port(port);
    }

    fn client_idx(&self, handle: ClientHandle) -> Option<usize> {
        self.clients()
            .iter::<ClientObject>()
            .position(|c| c.ok().map(|c| c.handle() == handle).unwrap_or_default())
    }

    pub(super) fn delete_client(&self, handle: ClientHandle) {
        let Some(idx) = self.client_idx(handle) else {
            log::warn!("could not find client with handle {handle}");
            return;
        };

        self.clients().remove(idx as u32);
        if self.clients().n_items() == 0 {
            self.update_placeholder_visibility();
        }
    }

    pub(super) fn update_client_config(&self, handle: ClientHandle, client: ClientConfig) {
        let Some(row) = self.row_for_handle(handle) else {
            log::warn!("could not find row for handle {handle}");
            return;
        };
        row.set_hostname(client.hostname);
        row.set_port(client.port);
        row.set_position(client.pos);
        row.set_clipboard_send(client.clipboard_send);
    }

    pub(super) fn update_client_state(&self, handle: ClientHandle, state: ClientState) {
        let Some(row) = self.row_for_handle(handle) else {
            log::warn!("could not find row for handle {handle}");
            return;
        };
        let Some(client_object) = self.client_object_for_handle(handle) else {
            log::warn!("could not find row for handle {handle}");
            return;
        };

        /* activation state */
        row.set_active(state.active);

        /* dns state */
        client_object.set_resolving(state.resolving);

        self.update_dns_state(handle, !state.ips.is_empty());
        let ips = state
            .ips
            .into_iter()
            .map(|ip| ip.to_string())
            .collect::<Vec<_>>();
        client_object.set_ips(ips);

        /* peer build version (drives the version-match indicator) */
        client_object.set_property(
            "peer-commit",
            crate::client_object::peer_commit_to_string(state.peer_commit),
        );
        row.refresh_version_status();
    }

    fn client_object_for_handle(&self, handle: ClientHandle) -> Option<ClientObject> {
        self.client_idx(handle)
            .and_then(|i| self.client_by_idx(i as u32))
    }

    fn row_for_handle(&self, handle: ClientHandle) -> Option<ClientRow> {
        self.client_idx(handle)
            .and_then(|i| self.row_by_idx(i as i32))
    }

    fn update_dns_state(&self, handle: ClientHandle, resolved: bool) {
        if let Some(client_row) = self.row_for_handle(handle) {
            client_row.set_dns_state(resolved);
        }
    }

    fn request_port_change(&self) {
        let port = self
            .imp()
            .port_entry
            .get()
            .text()
            .as_str()
            .parse::<u16>()
            .unwrap_or(DEFAULT_PORT);
        self.request(FrontendRequest::ChangePort(port));
    }

    fn request_capture(&self) {
        self.request(FrontendRequest::EnableCapture);
    }

    fn request_emulation(&self) {
        self.request(FrontendRequest::EnableEmulation);
    }

    fn request_client_create(&self) {
        self.request(FrontendRequest::Create);
    }

    pub(super) fn request_release_threshold(&self, threshold: u32) {
        self.request(FrontendRequest::SetReleaseThreshold(threshold));
    }

    pub(super) fn request_mdns_discovery(&self, enabled: bool) {
        self.request(FrontendRequest::SetMdnsDiscovery(enabled));
    }

    /// Forward the daemon's running-apps snapshot to the modal (if
    /// it's been created). No-op when the user hasn't opened the
    /// privacy window yet, since the picker is built lazily.
    pub(super) fn set_running_apps(&self, apps: Vec<lan_mouse_ipc::RunningApp>) {
        if let Some(window) = self.imp().clipboard_privacy_window.borrow().as_ref() {
            window.set_running_apps(apps);
        }
    }

    /// Replace the cached suppression list (host-OS strings) and
    /// update both the main-window subtitle and the modal (if
    /// open).
    pub(super) fn set_suppressed_apps(&self, apps: Vec<String>) {
        let imp = self.imp();
        imp.suppressed_apps.replace(apps.clone());
        let count = apps.len();
        let subtitle = match count {
            0 => "0 apps".to_owned(),
            1 => "1 app".to_owned(),
            n => format!("{n} apps"),
        };
        imp.clipboard_privacy_row.set_subtitle(&subtitle);
        if let Some(window) = imp.clipboard_privacy_window.borrow().as_ref() {
            window.set_apps(apps);
        }
    }

    /// Show (or re-present) the clipboard-privacy modal, populating
    /// it with the current suppression list. The modal is created
    /// on first open and reused thereafter so the user's in-progress
    /// edits aren't blown away by the every-toggle
    /// SuppressedAppsUpdated round-trip.
    pub(super) fn open_clipboard_privacy_window(&self) {
        let imp = self.imp();
        if imp.clipboard_privacy_window.borrow().is_none() {
            let window = ClipboardPrivacyWindow::new();
            window.set_transient_for(Some(self));
            // Match the parent-relative sizing the other dialogs use.
            let parent_w = self.width();
            if parent_w > 0 {
                let popup_w = (parent_w - 40).clamp(280, 700);
                window.set_default_width(popup_w);
            }
            window.connect_closure(
                "request-add",
                false,
                closure_local!(
                    #[strong(rename_to = parent)]
                    self,
                    move |_w: ClipboardPrivacyWindow, value: String| {
                        parent.request(FrontendRequest::AddSuppressedApp(value));
                    }
                ),
            );
            window.connect_closure(
                "request-remove",
                false,
                closure_local!(
                    #[strong(rename_to = parent)]
                    self,
                    move |_w: ClipboardPrivacyWindow, value: String| {
                        parent.request(FrontendRequest::RemoveSuppressedApp(value));
                    }
                ),
            );
            window.set_apps(imp.suppressed_apps.borrow().clone());
            // The daemon (a forked LSUIElement child) can't see
            // other apps via NSWorkspace / NSRunningApplication —
            // those APIs are scoped to the caller's loginwindow
            // session and the daemon doesn't fully inherit one.
            // The GUI process IS Aqua-attached, so we enumerate
            // here and skip the IPC roundtrip entirely.
            window.set_running_apps(input_capture::frontmost_app::list_running_apps());
            // Auto-refresh every 5s while the modal is visible so
            // launches/quits surface eventually without thrashing
            // the main thread (each refresh re-encodes ~30 PNG
            // icons). Skip refresh while the picker's popover is
            // open so the user's selection / search doesn't
            // disappear mid-interaction. Timer self-detaches when
            // the window is dropped.
            let window_weak = window.downgrade();
            glib::source::timeout_add_local(std::time::Duration::from_secs(5), move || {
                let Some(window) = window_weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if !window.is_visible() {
                    return glib::ControlFlow::Continue;
                }
                if window.picker_is_open() {
                    return glib::ControlFlow::Continue;
                }
                window.set_running_apps(input_capture::frontmost_app::list_running_apps());
                glib::ControlFlow::Continue
            });
            imp.clipboard_privacy_window.replace(Some(window));
        } else if let Some(window) = imp.clipboard_privacy_window.borrow().as_ref() {
            // Refresh both the suppressed-apps list and the
            // running-apps picker in case they changed while the
            // modal was hidden.
            window.set_apps(imp.suppressed_apps.borrow().clone());
            window.set_running_apps(input_capture::frontmost_app::list_running_apps());
        }
        if let Some(window) = imp.clipboard_privacy_window.borrow().as_ref() {
            window.present();
        }
    }

    fn open_fingerprint_dialog(&self, fp: Option<String>) {
        let window = FingerprintWindow::new(fp);
        window.set_transient_for(Some(self));
        // Size the popup 40 px narrower than the parent so it stays
        // fully visible when the parent has been tiled into a narrow
        // split (Hyprland and similar). Falls back to the XML default
        // (460 px) when the parent's allocated width isn't yet known
        // and clamps to the XML width-request floor.
        let parent_w = self.width();
        if parent_w > 0 {
            let popup_w = (parent_w - 40).clamp(280, 460);
            window.set_default_width(popup_w);
        }
        window.connect_closure(
            "confirm-clicked",
            false,
            closure_local!(
                #[strong(rename_to = parent)]
                self,
                move |w: FingerprintWindow, desc: String, fp: String| {
                    parent.request_fingerprint_add(desc, fp);
                    w.close();
                }
            ),
        );
        window.present();
    }

    fn request_fingerprint_add(&self, desc: String, fp: String) {
        self.request(FrontendRequest::AuthorizeKey(desc, fp));
    }

    fn request_fingerprint_remove(&self, fp: String) {
        self.request(FrontendRequest::RemoveAuthorizedKey(fp));
    }

    fn request(&self, request: FrontendRequest) {
        let mut requester = self.imp().frontend_request_writer.borrow_mut();
        let requester = requester.as_mut().unwrap();
        if let Err(e) = requester.request(request) {
            log::error!("error sending message: {e}");
        };
    }

    pub(super) fn show_toast(&self, msg: &str) {
        let toast = adw::Toast::new(msg);
        self.add_toast(toast);
    }

    pub(super) fn add_toast(&self, toast: adw::Toast) {
        let toast_overlay = &self.imp().toast_overlay;
        toast_overlay.add_toast(toast);
    }

    pub(super) fn set_capture(&self, active: bool) {
        self.imp().capture_active.replace(active);
        self.update_capture_emulation_status();
    }

    pub(super) fn set_emulation(&self, active: bool) {
        self.imp().emulation_active.replace(active);
        self.update_capture_emulation_status();
    }

    pub(super) fn set_mdns_discovery(&self, enabled: bool) {
        let imp = self.imp();
        let switch = &imp.mdns_discovery_switch;
        let handler = imp.mdns_discovery_handler.borrow();
        if let Some(id) = handler.as_ref() {
            switch.block_signal(id);
        }
        switch.set_active(enabled);
        switch.set_state(enabled);
        if let Some(id) = handler.as_ref() {
            switch.unblock_signal(id);
        }
    }

    pub(super) fn set_release_threshold(&self, threshold: u32) {
        let imp = self.imp();
        // Block the value-changed handler so programmatically setting
        // the slider value (e.g. on Sync from the daemon) doesn't
        // ricochet back as a SetReleaseThreshold request.
        let scale = &imp.release_threshold_scale;
        let handler_id = imp.release_threshold_handler.borrow();
        if let Some(id) = handler_id.as_ref() {
            scale.block_signal(id);
        }
        scale.set_value(threshold as f64);
        if let Some(id) = handler_id.as_ref() {
            scale.unblock_signal(id);
        }
        let label = if threshold == 0 {
            "Disabled".to_string()
        } else {
            format!("{threshold} px")
        };
        imp.release_threshold_value.set_label(&label);
    }

    #[cfg(target_os = "macos")]
    pub(super) fn refresh_capture_emulation_status(&self) {
        self.update_capture_emulation_status();
    }

    fn update_capture_emulation_status(&self) {
        let capture = self.imp().capture_active.get();
        let emulation = self.imp().emulation_active.get();

        #[cfg(target_os = "macos")]
        {
            // On macOS, capture and emulation share the same TCC gate
            // (Accessibility). Collapse to a single warning row —
            // emulation_status_row stays hidden and capture_status_row
            // doubles as the shared status indicator. Its text and
            // button mutate based on whether we're waiting for AX or
            // waiting for the user to relaunch the app.
            let anything_off = !capture || !emulation;
            self.imp().emulation_status_row.set_visible(false);
            self.imp().capture_status_row.set_visible(anything_off);
            self.imp().capture_emulation_group.set_visible(anything_off);

            if anything_off {
                self.update_macos_warning_row_text();
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            self.imp().capture_status_row.set_visible(!capture);
            self.imp().emulation_status_row.set_visible(!emulation);
            self.imp()
                .capture_emulation_group
                .set_visible(!capture || !emulation);
        }
    }

    #[cfg(target_os = "macos")]
    fn update_macos_warning_row_text(&self) {
        let row = &self.imp().capture_status_row;
        let button = &self.imp().input_capture_button;

        if crate::macos_privacy::accessibility_granted() {
            // AX granted but capture/emulation still off → the daemon
            // subprocess bailed at startup and needs a fresh process to
            // re-initialize with the new grant in place.
            row.set_title("Relaunch Required");
            row.set_subtitle("Accessibility granted — restart to activate capture and emulation.");
            set_button_content_label(button, "Relaunch");
        } else {
            // AX missing → send the user to System Settings.
            row.set_title("Input Capture Disabled");
            row.set_subtitle("Grant accessibility permission to enable.");
            set_button_content_label(button, "Grant");
        }
    }

    pub(super) fn set_authorized_keys(
        &self,
        mut fingerprints: HashMap<String, lan_mouse_ipc::IncomingPeerConfig>,
    ) {
        let authorized = self.authorized();
        // In-place diff: a full `remove_all` + rebuild would collapse
        // every expanded row whenever any setting changes (each
        // setting toggle round-trips back as `AuthorizedUpdated`).
        // Instead, walk the existing rows backward (so removals don't
        // shift indices), update matching KeyObjects in place, drop
        // entries no longer in the new map, and append anything
        // unmatched at the end. KeyRow tracks property-notify on the
        // KeyObject so the per-row widgets reflect the updates.
        let mut idx = authorized.n_items();
        while idx > 0 {
            idx -= 1;
            let Some(obj) = authorized.item(idx) else {
                continue;
            };
            let Ok(key_obj) = obj.downcast::<KeyObject>() else {
                continue;
            };
            let fp = key_obj.get_fingerprint();
            if let Some(peer) = fingerprints.remove(&fp) {
                key_obj.set_description(peer.description);
                key_obj.set_natural_scroll(peer.natural_scroll);
                key_obj.set_mouse_sensitivity(peer.mouse_sensitivity);
                key_obj.set_last_addr(peer.last_addr.unwrap_or_default());
                key_obj.set_last_hostname(peer.last_hostname.unwrap_or_default());
                key_obj.set_clipboard_receive(peer.clipboard_receive);
            } else {
                authorized.remove(idx);
            }
        }
        // Anything still in `fingerprints` is newly authorized.
        for (fingerprint, peer) in fingerprints {
            let key_obj = KeyObject::new(fingerprint, peer);
            authorized.append(&key_obj);
        }
        self.update_auth_placeholder_visibility();
    }

    pub(super) fn set_pk_fp(&self, fingerprint: &str) {
        self.imp().fingerprint_row.set_subtitle(fingerprint);
    }

    pub(super) fn request_authorization(&self, fingerprint: &str) {
        if let Some(w) = self.imp().authorization_window.borrow_mut().take() {
            w.close();
        }
        let window = AuthorizationWindow::new(fingerprint);
        window.set_transient_for(Some(self));
        // Same parent-relative sizing as the fingerprint dialog —
        // 40 px narrower than the parent, capped at 460 to match the
        // Add-Certificate modal, floored at 280 so it doesn't shrink
        // under content.
        let parent_w = self.width();
        if parent_w > 0 {
            let popup_w = (parent_w - 40).clamp(280, 460);
            window.set_default_width(popup_w);
        }
        window.connect_closure(
            "confirm-clicked",
            false,
            closure_local!(
                #[strong(rename_to = parent)]
                self,
                move |w: AuthorizationWindow, fp: String| {
                    w.close();
                    parent.open_fingerprint_dialog(Some(fp));
                }
            ),
        );
        window.connect_closure(
            "cancel-clicked",
            false,
            closure_local!(move |w: AuthorizationWindow| {
                w.close();
            }),
        );
        window.present();
        self.imp().authorization_window.replace(Some(window));
    }
}
