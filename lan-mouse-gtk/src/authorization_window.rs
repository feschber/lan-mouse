mod imp;

use glib::Object;
use gtk::{gio, glib, subclass::prelude::ObjectSubclassIsExt};

glib::wrapper! {
    pub struct AuthorizationWindow(ObjectSubclass<imp::AuthorizationWindow>)
    @extends adw::Window, gtk::Window, gtk::Widget,
    @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl AuthorizationWindow {
    pub(crate) fn new(fingerprint: &str) -> Self {
        let window: Self = Object::builder().build();
        window.imp().set_fingerprint(fingerprint);
        window
    }
}
