use std::cell::{Cell, RefCell};

use glib::Properties;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

#[derive(Properties)]
#[properties(wrapper_type = super::KeyObject)]
pub struct KeyObject {
    #[property(name = "description", get, set, type = String)]
    pub description: RefCell<String>,
    #[property(name = "fingerprint", get, set, type = String)]
    pub fingerprint: RefCell<String>,
    #[property(name = "natural-scroll", get, set, type = bool)]
    pub natural_scroll: Cell<bool>,
    #[property(name = "mouse-sensitivity", get, set, type = f64)]
    pub mouse_sensitivity: Cell<f64>,
    /// Most recent IP this peer connected from (or empty when
    /// they've never connected). Empty string is the GObject-
    /// idiomatic stand-in for `Option::None` since the property
    /// macro doesn't support `Option`.
    #[property(name = "last-addr", get, set, type = String)]
    pub last_addr: RefCell<String>,
    /// mDNS-discovered hostname for `last_addr`, if known. Empty
    /// = no hostname resolution available.
    #[property(name = "last-hostname", get, set, type = String)]
    pub last_hostname: RefCell<String>,
    /// Whether this peer's clipboard text should be applied to
    /// the local clipboard. Mirrors
    /// [`lan_mouse_ipc::IncomingPeerConfig::clipboard_receive`].
    #[property(name = "clipboard-receive", get, set, type = bool)]
    pub clipboard_receive: Cell<bool>,
}

impl Default for KeyObject {
    fn default() -> Self {
        Self {
            description: RefCell::new(String::new()),
            fingerprint: RefCell::new(String::new()),
            natural_scroll: Cell::new(false),
            mouse_sensitivity: Cell::new(1.0),
            last_addr: RefCell::new(String::new()),
            last_hostname: RefCell::new(String::new()),
            clipboard_receive: Cell::new(false),
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for KeyObject {
    const NAME: &'static str = "KeyObject";
    type Type = super::KeyObject;
}

#[glib::derived_properties]
impl ObjectImpl for KeyObject {}
