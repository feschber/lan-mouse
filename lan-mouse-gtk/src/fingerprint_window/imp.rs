use std::sync::OnceLock;

use adw::prelude::*;
use adw::subclass::prelude::*;
use glib::subclass::InitializingObject;
use gtk::{
    Button, CompositeTemplate, Entry,
    glib::{self, subclass::Signal},
    template_callbacks,
};

#[derive(CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/fingerprint_window.ui")]
pub struct FingerprintWindow {
    #[template_child]
    pub description: TemplateChild<Entry>,
    #[template_child]
    pub fingerprint: TemplateChild<Entry>,
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
        let desc = self.description.text().as_str().trim().to_owned();
        let fp = self.fingerprint.text().as_str().trim().to_owned();
        // Defensive guard: the button is wired to be insensitive while
        // either field is empty (see `constructed`), but a user could
        // theoretically trigger the action via the keyboard accelerator
        // before the validity recompute finishes. Drop empty submits.
        if desc.is_empty() || fp.is_empty() {
            return;
        }
        self.obj().emit_by_name("confirm-clicked", &[&desc, &fp])
    }
}

impl FingerprintWindow {
    fn both_filled(&self) -> bool {
        !self.description.text().trim().is_empty() && !self.fingerprint.text().trim().is_empty()
    }

    fn refresh_confirm_sensitivity(&self) {
        self.confirm_button.set_sensitive(self.both_filled());
    }
}

impl ObjectImpl for FingerprintWindow {
    fn signals() -> &'static [Signal] {
        static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
        SIGNALS.get_or_init(|| {
            vec![
                Signal::builder("confirm-clicked")
                    .param_types([String::static_type(), String::static_type()])
                    .build(),
            ]
        })
    }

    fn constructed(&self) {
        self.parent_constructed();

        // Confirm is disabled while either Description or
        // SHA-256 Fingerprint is empty (after trim). Recompute on
        // every keystroke via the GtkEditable `changed` signal.
        self.refresh_confirm_sensitivity();

        let obj = self.obj();
        let weak_desc = obj.downgrade();
        self.description.connect_changed(move |_| {
            if let Some(o) = weak_desc.upgrade() {
                o.imp().refresh_confirm_sensitivity();
            }
        });
        let weak_fp = obj.downgrade();
        self.fingerprint.connect_changed(move |_| {
            if let Some(o) = weak_fp.upgrade() {
                o.imp().refresh_confirm_sensitivity();
            }
        });
    }
}

impl WidgetImpl for FingerprintWindow {}
impl WindowImpl for FingerprintWindow {}
impl ApplicationWindowImpl for FingerprintWindow {}
impl AdwWindowImpl for FingerprintWindow {}
