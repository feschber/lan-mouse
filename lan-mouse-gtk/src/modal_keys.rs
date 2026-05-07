//! Shared keyboard shortcuts for modal windows: `Escape` and
//! `Cmd+W` (macOS) / `Ctrl+W` (Linux/Windows) both dismiss the
//! modal.
//!
//! Application-level accelerators (`app.set_accels_for_action`)
//! only deliver to GtkApplicationWindow children. Our modals are
//! plain AdwWindow, so without an explicit per-modal key
//! controller, `Cmd+W` falls through to the focused
//! ApplicationWindow (the main window) and closes that instead of
//! the modal — exactly the wrong UX.
//!
//! Both shortcuts run in the bubble phase (the
//! `EventControllerKey` default), so a child widget that handles
//! the key first — open AdwComboRow popover, focused search
//! entry, etc. — consumes it and our handler doesn't fire. That's
//! intentional: pressing Escape with the picker open dismisses
//! the picker, not the whole modal.

use gtk::{
    EventControllerKey, gdk,
    glib::{Propagation, object::IsA},
    prelude::*,
};

pub fn wire_close_shortcuts(window: &impl IsA<gtk::Window>) {
    let window: gtk::Window = window.clone().upcast();
    let key = EventControllerKey::new();
    let weak = window.downgrade();
    key.connect_key_pressed(move |_, keyval, _, modifier| {
        let Some(w) = weak.upgrade() else {
            return Propagation::Proceed;
        };
        if keyval == gdk::Key::Escape {
            w.close();
            return Propagation::Stop;
        }
        let key_is_w = keyval == gdk::Key::w || keyval == gdk::Key::W;
        let close_modifier = if cfg!(target_os = "macos") {
            modifier.contains(gdk::ModifierType::META_MASK)
        } else {
            modifier.contains(gdk::ModifierType::CONTROL_MASK)
        };
        if key_is_w && close_modifier {
            w.close();
            return Propagation::Stop;
        }
        Propagation::Proceed
    });
    window.add_controller(key);
}
