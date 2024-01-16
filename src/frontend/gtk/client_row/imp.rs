use std::cell::RefCell;

use adw::subclass::prelude::*;
use adw::{prelude::*, ActionRow, ComboRow};
use glib::{subclass::InitializingObject, Binding};
use gtk::glib::clone;
use gtk::glib::once_cell::sync::Lazy;
use gtk::glib::subclass::Signal;
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
    const ABSTRACT: bool = false;

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

    fn signals() -> &'static [glib::subclass::Signal] {
        static SIGNALS: Lazy<Vec<Signal>> = Lazy::new(|| {
            vec![
                Signal::builder("request-update")
                    .param_types([bool::static_type()])
                    .build(),
                Signal::builder("request-delete").build(),
            ]
        });
        SIGNALS.as_ref()
    }
}

#[gtk::template_callbacks]
impl ClientRow {
    #[template_callback]
    fn handle_client_set_state(&self, state: bool, _switch: &Switch) -> bool {
        log::debug!("state change -> requesting update");
        self.obj().emit_by_name::<()>("request-update", &[&state]);
        true // dont run default handler
    }

    #[template_callback]
    fn handle_client_delete(&self, _button: &Button) {
        log::debug!("delete button pressed -> requesting delete");
        self.obj().emit_by_name::<()>("request-delete", &[]);
    }
}

impl WidgetImpl for ClientRow {}
impl BoxImpl for ClientRow {}
impl ListBoxRowImpl for ClientRow {}
impl PreferencesRowImpl for ClientRow {}
impl ExpanderRowImpl for ClientRow {}
