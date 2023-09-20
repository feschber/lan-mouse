use std::cell::RefCell;

use glib::{Binding, subclass::InitializingObject};
use adw::{prelude::*, ComboRow};
use adw::subclass::prelude::*;
use gtk::glib::Properties;
use gtk::{glib, CompositeTemplate, Switch, Button};

use crate::frontend::gtk::client_object::ClientObject;

#[derive(Properties, CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/client_row.ui")]
#[properties(wrapper_type = super::ClientRow)]
pub struct ClientRow {
    #[template_child]
    pub enable_switch: TemplateChild<gtk::Switch>,
    #[template_child]
    pub hostname: TemplateChild<gtk::Entry>,
    #[template_child]
    pub port: TemplateChild<gtk::Entry>,
    #[template_child]
    pub position: TemplateChild<ComboRow>,
    #[template_child]
    pub delete_button: TemplateChild<gtk::Button>,
    pub bindings: RefCell<Vec<Binding>>,
    #[property(get, set)]
    client_object: RefCell<Option<ClientObject>>,
}

#[glib::object_subclass]
impl ObjectSubclass for ClientRow {
    // `NAME` needs to match `class` attribute of template
    const NAME: &'static str = "ClientRow";
    type Type = super::ClientRow;
    type ParentType = adw::ExpanderRow;

    fn class_init(klass: &mut Self::Class) {
        klass.bind_template();
        klass.bind_template_callbacks();
    }

    fn instance_init(obj: &InitializingObject<Self>) {
        obj.init_template();
    }
}

impl ObjectImpl for ClientRow {
    fn constructed(&self) {
        self.parent_constructed();
    }
}

#[gtk::template_callbacks]
impl ClientRow {
    #[template_callback]
    fn handle_client_set_state(&self, state: bool, switch: &Switch) -> bool {
        let idx = self.obj().index();
        switch.activate_action("win.activate-client", Some(&idx.to_variant())).unwrap();
        switch.set_state(state);

        true // dont run default handler
    }

    #[template_callback]
    fn handle_client_delete(&self, button: &Button) {
        let idx = self.obj().index();
        button.activate_action("win.delete-client", Some(&idx.to_variant())).unwrap();
    }
}

impl WidgetImpl for ClientRow {}
impl BoxImpl for ClientRow {}
impl ListBoxRowImpl for ClientRow {}
impl PreferencesRowImpl for ClientRow {}
impl ExpanderRowImpl for ClientRow {}
