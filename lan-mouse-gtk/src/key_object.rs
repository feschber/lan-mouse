mod imp;

use adw::subclass::prelude::*;
use gtk::glib::{self, Object};
use lan_mouse_ipc::IncomingPeerConfig;

glib::wrapper! {
    pub struct KeyObject(ObjectSubclass<imp::KeyObject>);
}

impl KeyObject {
    pub fn new(fingerprint: String, peer: IncomingPeerConfig) -> Self {
        Object::builder()
            .property("description", peer.description)
            .property("fingerprint", fingerprint)
            .property("natural-scroll", peer.natural_scroll)
            .property("mouse-sensitivity", peer.mouse_sensitivity)
            .property("last-addr", peer.last_addr.unwrap_or_default())
            .property("last-hostname", peer.last_hostname.unwrap_or_default())
            .property("clipboard-receive", peer.clipboard_receive)
            .build()
    }

    pub fn get_description(&self) -> String {
        self.imp().description.borrow().clone()
    }

    pub fn get_fingerprint(&self) -> String {
        self.imp().fingerprint.borrow().clone()
    }
}
