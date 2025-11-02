use std::sync::OnceLock;

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::subclass::InitializingObject;
use gtk::{
    Button, CompositeTemplate, Label,
    glib::{self, subclass::Signal},
    template_callbacks,
};

#[derive(CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/authorization_window.ui")]
pub struct AuthorizationWindow {
    #[template_child]
    pub fingerprint: TemplateChild<Label>,
    #[template_child]
    pub cancel_button: TemplateChild<Button>,
    #[template_child]
    pub confirm_button: TemplateChild<Button>,
}

#[glib::object_subclass]
impl ObjectSubclass for AuthorizationWindow {
    const NAME: &'static str = "AuthorizationWindow";
    const ABSTRACT: bool = false;

    type Type = super::AuthorizationWindow;
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
impl AuthorizationWindow {
    #[template_callback]
    fn handle_confirm(&self, _button: Button) {
        let fp = self.fingerprint.text().as_str().trim().to_owned();
        self.obj().emit_by_name("confirm-clicked", &[&fp])
    }

    #[template_callback]
    fn handle_cancel(&self, _: Button) {
        self.obj().emit_by_name("cancel-clicked", &[])
    }

    pub(super) fn set_fingerprint(&self, fingerprint: &str) {
        self.fingerprint.set_text(fingerprint);
    }
}

impl ObjectImpl for AuthorizationWindow {
    fn signals() -> &'static [Signal] {
        static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
        SIGNALS.get_or_init(|| {
            vec![
                Signal::builder("confirm-clicked")
                    .param_types([String::static_type()])
                    .build(),
                Signal::builder("cancel-clicked").build(),
            ]
        })
    }
}

impl WidgetImpl for AuthorizationWindow {}
impl WindowImpl for AuthorizationWindow {}
impl ApplicationWindowImpl for AuthorizationWindow {}
impl AdwWindowImpl for AuthorizationWindow {}
