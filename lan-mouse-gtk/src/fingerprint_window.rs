mod imp;

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::{clone, Object};
use gtk::{
    gio,
    glib::{self, closure_local},
    ListBox, NoSelection,
};

glib::wrapper! {
    pub struct FingerprintWindow(ObjectSubclass<imp::FingerprintWindow>)
    @extends adw::Window, gtk::Window, gtk::Widget,
    @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl FingerprintWindow {
    pub(crate) fn new() -> Self {
        let window: Self = Object::builder().build();
        window
    }
}
