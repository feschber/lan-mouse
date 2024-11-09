use std::cell::RefCell;

use adw::subclass::prelude::*;
use adw::{prelude::*, ActionRow};
use glib::{subclass::InitializingObject, Binding};
use gtk::glib::clone;
use gtk::glib::subclass::Signal;
use gtk::{glib, Button, CompositeTemplate};
use std::sync::OnceLock;

#[derive(CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/key_row.ui")]
pub struct KeyRow {
    #[template_child]
    pub delete_button: TemplateChild<gtk::Button>,
    pub bindings: RefCell<Vec<Binding>>,
}

#[glib::object_subclass]
impl ObjectSubclass for KeyRow {
    // `NAME` needs to match `class` attribute of template
    const NAME: &'static str = "KeyRow";
    const ABSTRACT: bool = false;

    type Type = super::KeyRow;
    type ParentType = ActionRow;

    fn class_init(klass: &mut Self::Class) {
        klass.bind_template();
        klass.bind_template_callbacks();
    }

    fn instance_init(obj: &InitializingObject<Self>) {
        obj.init_template();
    }
}

impl ObjectImpl for KeyRow {
    fn constructed(&self) {
        self.parent_constructed();
        self.delete_button.connect_clicked(clone!(
            #[weak(rename_to = row)]
            self,
            move |button| {
                row.handle_delete(button);
            }
        ));
    }

    fn signals() -> &'static [glib::subclass::Signal] {
        static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
        SIGNALS.get_or_init(|| vec![Signal::builder("request-delete").build()])
    }
}

#[gtk::template_callbacks]
impl KeyRow {
    #[template_callback]
    fn handle_delete(&self, _button: &Button) {
        self.obj().emit_by_name::<()>("request-delete", &[]);
    }
}

impl WidgetImpl for KeyRow {}
impl BoxImpl for KeyRow {}
impl ListBoxRowImpl for KeyRow {}
impl PreferencesRowImpl for KeyRow {}
impl ActionRowImpl for KeyRow {}
