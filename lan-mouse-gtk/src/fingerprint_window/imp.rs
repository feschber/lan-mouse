use std::sync::OnceLock;

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::subclass::InitializingObject;
use gtk::{
    glib::{self, subclass::Signal},
    template_callbacks, Button, CompositeTemplate, Text,
};

#[derive(CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/fingerprint_window.ui")]
pub struct FingerprintWindow {
    #[template_child]
    pub description: TemplateChild<Text>,
    #[template_child]
    pub fingerprint: TemplateChild<Text>,
    #[template_child]
    pub confirm_button: TemplateChild<Button>,
}

#[glib::object_subclass]
impl ObjectSubclass for FingerprintWindow {
    const NAME: &'static str = "FingerprintWindow";
    const ABSTRACT: bool = false;

    type Type = super::FingerprintWindow;
    type ParentType = adw::Window;

    fn class_init(klass: &mut Self::Class) {
        klass.bind_template();
        klass.bind_template_callbacks();
    }

    fn instance_init(obj: &InitializingObject<Self>) {
        obj.init_template();
    }
}

#[template_callbacks]
impl FingerprintWindow {
    #[template_callback]
    fn handle_confirm(&self, _button: Button) {
        let desc = self.description.text().to_string();
        let fp = self.fingerprint.text().to_string();
        self.obj().emit_by_name("confirm-clicked", &[&fp, &desc])
    }
}

impl ObjectImpl for FingerprintWindow {
    fn signals() -> &'static [Signal] {
        static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
        SIGNALS.get_or_init(|| {
            vec![Signal::builder("confirm-clicked")
                .param_types([String::static_type(), String::static_type()])
                .build()]
        })
    }
}

impl WidgetImpl for FingerprintWindow {}
impl WindowImpl for FingerprintWindow {}
impl ApplicationWindowImpl for FingerprintWindow {}
impl AdwWindowImpl for FingerprintWindow {}
