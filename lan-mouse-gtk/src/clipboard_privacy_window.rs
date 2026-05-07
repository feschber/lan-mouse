mod imp;

use glib::Object;
use gtk::{gio, glib, subclass::prelude::*};
use lan_mouse_ipc::AppIdent;

pub use imp::pair_to_app_ident;

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

    /// Replace the displayed list with `apps` and rebuild the UI.
    /// Called from `Window::set_suppressed_apps` when the daemon
    /// pushes a [`lan_mouse_ipc::FrontendEvent::SuppressedAppsUpdated`].
    pub fn set_apps(&self, apps: Vec<AppIdent>) {
        self.imp().apps.replace(apps);
        self.imp().rebuild_list();
    }
}

impl Default for ClipboardPrivacyWindow {
    fn default() -> Self {
        Self::new()
    }
}
