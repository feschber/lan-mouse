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
}

impl Default for KeyObject {
    fn default() -> Self {
        Self {
            description: RefCell::new(String::new()),
            fingerprint: RefCell::new(String::new()),
            natural_scroll: Cell::new(false),
            mouse_sensitivity: Cell::new(1.0),
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
