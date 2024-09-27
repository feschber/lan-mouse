use std::cell::RefCell;

use glib::Properties;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

#[derive(Properties, Default)]
#[properties(wrapper_type = super::KeyObject)]
pub struct KeyObject {
    #[property(name = "fingerprint", get, set, type = String)]
    pub fingerprint: RefCell<String>,
}

#[glib::object_subclass]
impl ObjectSubclass for KeyObject {
    const NAME: &'static str = "KeyObject";
    type Type = super::KeyObject;
}

#[glib::derived_properties]
impl ObjectImpl for KeyObject {}
