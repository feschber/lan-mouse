use std::cell::RefCell;

use glib::{Binding, subclass::InitializingObject};
use adw::{prelude::*, ComboRow};
use adw::subclass::prelude::*;
use gtk::{glib, CompositeTemplate, Switch};

#[derive(CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/client_row.ui")]
pub struct ClientRow {
    #[template_child]
    pub enable_switch: TemplateChild<gtk::Switch>,
    #[template_child]
    pub hostname: TemplateChild<gtk::Entry>,
    #[template_child]
    pub port: TemplateChild<gtk::Entry>,
    #[template_child]
    pub position: TemplateChild<ComboRow>,
    pub bindings: RefCell<Vec<Binding>>,
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
        if state {
            log::info!("activate");
        } else {
            log::info!("deactivate");
        }
        log::info!("{:?}", self.hostname);

        let idx: u32 = 0;
        switch.activate_action("win.activate-client", Some(&idx.to_variant())).unwrap();

        switch.set_state(state);
        // dont run default handler
        true
    }
}

impl WidgetImpl for ClientRow {}
impl BoxImpl for ClientRow {}
impl ListBoxRowImpl for ClientRow {}
impl PreferencesRowImpl for ClientRow {}
impl ExpanderRowImpl for ClientRow {}
