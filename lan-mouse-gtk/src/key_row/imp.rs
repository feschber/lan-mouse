use std::cell::RefCell;

use adw::subclass::prelude::*;
use adw::{ExpanderRow, prelude::*};
use glib::{Binding, subclass::InitializingObject};
use gtk::glib::clone;
use gtk::glib::subclass::Signal;
use gtk::{Button, CompositeTemplate, SpinButton, Switch, glib};
use std::sync::OnceLock;

#[derive(CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/key_row.ui")]
pub struct KeyRow {
    #[template_child]
    pub delete_button: TemplateChild<Button>,
    #[template_child]
    pub natural_scroll_switch: TemplateChild<Switch>,
    #[template_child]
    pub sensitivity_spin: TemplateChild<SpinButton>,
    pub bindings: RefCell<Vec<Binding>>,
    /// Signal-handler IDs for the per-row controls. Used to
    /// `block_signal` while pushing daemon-driven state into the
    /// widgets so we don't ping-pong a server-originated update
    /// back as a fresh user request.
    pub natural_scroll_handler: RefCell<Option<glib::SignalHandlerId>>,
    pub sensitivity_handler: RefCell<Option<glib::SignalHandlerId>>,
}

#[glib::object_subclass]
impl ObjectSubclass for KeyRow {
    const NAME: &'static str = "KeyRow";
    const ABSTRACT: bool = false;

    type Type = super::KeyRow;
    type ParentType = ExpanderRow;

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

        let natural_scroll_handler = self.natural_scroll_switch.connect_state_set(clone!(
            #[weak(rename_to = row)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, state| {
                row.obj()
                    .emit_by_name::<()>("request-natural-scroll-change", &[&state]);
                glib::Propagation::Proceed
            }
        ));
        self.natural_scroll_handler
            .replace(Some(natural_scroll_handler));

        let sensitivity_handler =
            self.sensitivity_spin
                .connect_value_changed(clone!(
                    #[weak(rename_to = row)]
                    self,
                    move |spin| {
                        row.obj()
                            .emit_by_name::<()>("request-sensitivity-change", &[&spin.value()]);
                    }
                ));
        self.sensitivity_handler.replace(Some(sensitivity_handler));
    }

    fn signals() -> &'static [glib::subclass::Signal] {
        static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
        SIGNALS.get_or_init(|| {
            vec![
                Signal::builder("request-delete").build(),
                Signal::builder("request-natural-scroll-change")
                    .param_types([bool::static_type()])
                    .build(),
                Signal::builder("request-sensitivity-change")
                    .param_types([f64::static_type()])
                    .build(),
            ]
        })
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
impl ExpanderRowImpl for KeyRow {}
