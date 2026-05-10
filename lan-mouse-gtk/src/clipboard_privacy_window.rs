mod imp;

use glib::Object;
use gtk::{gio, glib, subclass::prelude::*};
use lan_mouse_ipc::RunningApp;

glib::wrapper! {
    pub struct ClipboardPrivacyWindow(ObjectSubclass<imp::ClipboardPrivacyWindow>)
    @extends adw::Window, gtk::Window, gtk::Widget,
    @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl ClipboardPrivacyWindow {
    pub fn new() -> Self {
        Object::builder().build()
    }

    /// Replace the displayed list with the host-OS strings `apps`
    /// and rebuild the UI. Called from `Window::set_suppressed_apps`
    /// when the daemon pushes
    /// [`lan_mouse_ipc::FrontendEvent::SuppressedAppsUpdated`].
    /// Also re-applies the picker filter against the cached
    /// running-apps snapshot so a removed app reappears in the
    /// picker immediately, instead of waiting for the 5 s
    /// auto-refresh tick.
    pub fn set_apps(&self, apps: Vec<String>) {
        self.imp().apps.replace(apps);
        self.imp().rebuild_list();
        let cached = self.imp().last_running.borrow().clone();
        if !cached.is_empty() {
            self.imp().set_running_apps(cached);
        }
    }

    /// Replace the picker contents with the daemon-supplied
    /// running-apps snapshot.
    pub fn set_running_apps(&self, running: Vec<RunningApp>) {
        self.imp().set_running_apps(running);
    }

    /// True when the picker's popover is open. Used by the
    /// auto-refresh timer to skip thrashing the user's selection
    /// while they're interacting with the dropdown.
    pub fn picker_is_open(&self) -> bool {
        self.imp().picker_is_open()
    }
}

impl Default for ClipboardPrivacyWindow {
    fn default() -> Self {
        Self::new()
    }
}
