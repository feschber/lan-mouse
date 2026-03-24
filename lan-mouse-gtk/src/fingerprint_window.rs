mod imp;

use glib::Object;
use gtk::{gio, glib, prelude::ObjectExt, subclass::prelude::ObjectSubclassIsExt};

glib::wrapper! {
    pub struct FingerprintWindow(ObjectSubclass<imp::FingerprintWindow>)
    @extends adw::Window, gtk::Window, gtk::Widget,
    @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl FingerprintWindow {
    pub(crate) fn new(fingerprint: Option<String>) -> Self {
        let window: Self = Object::builder().build();
        if let Some(fp) = fingerprint {
            window.imp().fingerprint.set_property("text", fp);
            window.imp().fingerprint.set_property("editable", false);
        }
        window
    }
}
