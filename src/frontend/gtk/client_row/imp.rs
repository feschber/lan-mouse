use std::cell::RefCell;

use adw::subclass::prelude::*;
use adw::{prelude::*, ActionRow, ComboRow};
use glib::{subclass::InitializingObject, Binding};
use gtk::glib::clone;
use gtk::{glib, Button, CompositeTemplate, Switch};

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
    #[template_child]
    pub delete_row: TemplateChild<ActionRow>,
    #[template_child]
    pub delete_button: TemplateChild<gtk::Button>,
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
        self.delete_button
            .connect_clicked(clone!(@weak self as row => move |button| {
                row.handle_client_delete(button);
            }));
    }
}

#[gtk::template_callbacks]
impl ClientRow {
    #[template_callback]
    fn handle_client_set_state(&self, state: bool, switch: &Switch) -> bool {
        let idx = self.obj().index() as u32;
        if let Err(e) = switch.activate_action("win.request-client-update", Some(&idx.to_variant()))
        {
            log::error!("{e}");
        }
        switch.set_state(state);
        switch.set_active(state);

        true // dont run default handler
    }

    #[template_callback]
    fn handle_client_delete(&self, button: &Button) {
        log::debug!("delete button pressed");
        let idx = self.obj().index() as u32;
        button
            .activate_action("win.request-client-delete", Some(&idx.to_variant()))
            .unwrap();
    }
}

impl WidgetImpl for ClientRow {}
impl BoxImpl for ClientRow {}
impl ListBoxRowImpl for ClientRow {}
impl PreferencesRowImpl for ClientRow {}
impl ExpanderRowImpl for ClientRow {}
