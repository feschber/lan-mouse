mod imp;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib::{self, Object, clone};

use super::KeyObject;

glib::wrapper! {
    pub struct KeyRow(ObjectSubclass<imp::KeyRow>)
    @extends gtk::ListBoxRow, gtk::Widget, adw::PreferencesRow, adw::ActionRow, adw::ExpanderRow,
    @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for KeyRow {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyRow {
    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn bind(&self, key_object: &KeyObject) {
        let mut bindings = self.imp().bindings.borrow_mut();

        let title_binding = key_object
            .bind_property("description", self, "title")
            .sync_create()
            .build();
        bindings.push(title_binding);

        // The fingerprint moves from the title-row subtitle to a
        // dedicated row inside the expansion. The title-row subtitle
        // is now computed from last_hostname/last_addr (mDNS-resolved
        // identity) so users see something more useful at a glance.
        let fp_binding = key_object
            .bind_property("fingerprint", &self.imp().fingerprint_row.get(), "subtitle")
            .sync_create()
            .build();
        bindings.push(fp_binding);
        drop(bindings);

        // Initial widget state from KeyObject without firing the
        // user-change signals (no ping-pong on bind).
        self.refresh_natural_scroll_widget(key_object.natural_scroll());
        self.refresh_sensitivity_widget(key_object.mouse_sensitivity());
        self.refresh_clipboard_receive_widget(key_object.clipboard_receive());
        self.refresh_summary(
            key_object.natural_scroll(),
            key_object.mouse_sensitivity(),
            key_object.clipboard_receive(),
        );
        self.refresh_identity_subtitle(&key_object.last_hostname(), &key_object.last_addr());

        // Wire the copy-to-clipboard button. The handler reads the
        // fingerprint live from the KeyObject so an in-place
        // mutation (rare for fingerprint, but possible) is honored.
        let key_object_weak = key_object.downgrade();
        self.imp()
            .copy_fingerprint_button
            .connect_clicked(move |_| {
                let Some(obj) = key_object_weak.upgrade() else {
                    return;
                };
                if let Some(display) = gtk::gdk::Display::default() {
                    display.clipboard().set_text(&obj.fingerprint());
                }
            });

        // Track in-place mutations on KeyObject — `set_authorized_keys`
        // updates existing KeyObjects rather than rebuilding the list,
        // so we need to react to property-notify, not just bind-time
        // values.
        let mut handlers = self.imp().key_object_handlers.borrow_mut();
        let h = key_object.connect_natural_scroll_notify(clone!(
            #[weak(rename_to = row)]
            self,
            move |obj| {
                row.refresh_natural_scroll_widget(obj.natural_scroll());
                row.refresh_summary(
                    obj.natural_scroll(),
                    obj.mouse_sensitivity(),
                    obj.clipboard_receive(),
                );
            }
        ));
        handlers.push((key_object.clone(), h));

        let h = key_object.connect_mouse_sensitivity_notify(clone!(
            #[weak(rename_to = row)]
            self,
            move |obj| {
                row.refresh_sensitivity_widget(obj.mouse_sensitivity());
                row.refresh_summary(
                    obj.natural_scroll(),
                    obj.mouse_sensitivity(),
                    obj.clipboard_receive(),
                );
            }
        ));
        handlers.push((key_object.clone(), h));

        let h = key_object.connect_clipboard_receive_notify(clone!(
            #[weak(rename_to = row)]
            self,
            move |obj| {
                row.refresh_clipboard_receive_widget(obj.clipboard_receive());
                row.refresh_summary(
                    obj.natural_scroll(),
                    obj.mouse_sensitivity(),
                    obj.clipboard_receive(),
                );
            }
        ));
        handlers.push((key_object.clone(), h));

        let h = key_object.connect_last_hostname_notify(clone!(
            #[weak(rename_to = row)]
            self,
            move |obj| {
                row.refresh_identity_subtitle(&obj.last_hostname(), &obj.last_addr());
            }
        ));
        handlers.push((key_object.clone(), h));

        let h = key_object.connect_last_addr_notify(clone!(
            #[weak(rename_to = row)]
            self,
            move |obj| {
                row.refresh_identity_subtitle(&obj.last_hostname(), &obj.last_addr());
            }
        ));
        handlers.push((key_object.clone(), h));
    }

    pub fn unbind(&self) {
        for binding in self.imp().bindings.borrow_mut().drain(..) {
            binding.unbind();
        }
        for (obj, id) in self.imp().key_object_handlers.borrow_mut().drain(..) {
            obj.disconnect(id);
        }
    }

    fn refresh_natural_scroll_widget(&self, value: bool) {
        let imp = self.imp();
        let switch = &imp.natural_scroll_switch;
        let handler = imp.natural_scroll_handler.borrow();
        if let Some(id) = handler.as_ref() {
            switch.block_signal(id);
        }
        switch.set_active(value);
        switch.set_state(value);
        if let Some(id) = handler.as_ref() {
            switch.unblock_signal(id);
        }
    }

    fn refresh_sensitivity_widget(&self, value: f64) {
        let imp = self.imp();
        let spin = &imp.sensitivity_spin;
        let handler = imp.sensitivity_handler.borrow();
        if let Some(id) = handler.as_ref() {
            spin.block_signal(id);
        }
        spin.set_value(value);
        if let Some(id) = handler.as_ref() {
            spin.unblock_signal(id);
        }
    }

    fn refresh_clipboard_receive_widget(&self, value: bool) {
        let imp = self.imp();
        let switch = &imp.clipboard_receive_switch;
        let handler = imp.clipboard_receive_handler.borrow();
        if let Some(id) = handler.as_ref() {
            switch.block_signal(id);
        }
        switch.set_active(value);
        switch.set_state(value);
        if let Some(id) = handler.as_ref() {
            switch.unblock_signal(id);
        }
    }

    /// Compute the title-row subtitle from the peer's most recent
    /// connection identity. mDNS gives us a hostname most of the
    /// time, falling back to a bare IP, falling back to a "never
    /// connected" placeholder for pre-shared-trust authorizations
    /// that haven't been used yet.
    fn refresh_identity_subtitle(&self, hostname: &str, addr: &str) {
        let s = match (hostname.is_empty(), addr.is_empty()) {
            (true, true) => "(not yet connected)".to_owned(),
            (true, false) => addr.to_owned(),
            (false, true) => hostname.to_owned(),
            (false, false) => format!("{hostname} ({addr})"),
        };
        // Bypass trait-name ambiguity (both ActionRow and ExpanderRow
        // define `set_subtitle`) by going through GObject property
        // dispatch. `subtitle` resolves to ExpanderRow's title-row
        // subtitle, which is what we want.
        self.set_property("subtitle", s);
    }

    /// Update the title-row summary label so a collapsed row hints
    /// at non-default settings. Hidden when every field is at
    /// default so a freshly-authorized peer's row is uncluttered.
    fn refresh_summary(&self, natural_scroll: bool, sensitivity: f64, clipboard_receive: bool) {
        let label = &self.imp().settings_summary;
        let parts = format_summary_parts(natural_scroll, sensitivity, clipboard_receive);
        if parts.is_empty() {
            label.set_visible(false);
            label.set_text("");
        } else {
            label.set_text(&parts.join(" · "));
            label.set_visible(true);
        }
    }
}

/// Render the non-default settings as short tokens (e.g. `Natural`,
/// `1.5×`, `Clipboard`). Returns an empty Vec when every field is
/// at default.
fn format_summary_parts(
    natural_scroll: bool,
    sensitivity: f64,
    clipboard_receive: bool,
) -> Vec<String> {
    let mut parts = Vec::new();
    if natural_scroll {
        parts.push("Natural".to_owned());
    }
    if (sensitivity - 1.0).abs() > 1e-6 {
        parts.push(format_sensitivity(sensitivity));
    }
    if clipboard_receive {
        parts.push("Clipboard".to_owned());
    }
    parts
}

fn format_sensitivity(v: f64) -> String {
    // Match the spin-button's 2-digit precision but trim trailing
    // zeros and a dangling decimal point so 1.50 → "1.5×" and
    // 0.85 → "0.85×". 1.0 never reaches here (filtered above).
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0');
    let s = s.trim_end_matches('.');
    format!("{s}×")
}
