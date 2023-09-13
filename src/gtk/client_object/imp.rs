use std::cell::RefCell;

use glib::Properties;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use super::ClientData;

#[derive(Properties, Default)]
#[properties(wrapper_type = super::ClientObject)]
pub struct ClientObject {
    #[property(name = "hostname", get, set, type = String, member = hostname)]
    #[property(name = "port", get, set, type = u32, member = port)]
    #[property(name = "active", get, set, type = bool, member = active)]
    #[property(name = "position", get, set, type = String, member = position)]
    pub data: RefCell<ClientData>,
}

#[glib::object_subclass]
impl ObjectSubclass for ClientObject {
    const NAME: &'static str = "ClientObject";
    type Type = super::ClientObject;
}

#[glib::derived_properties]
impl ObjectImpl for ClientObject {}
