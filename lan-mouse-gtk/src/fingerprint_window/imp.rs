use adw::subclass::prelude::*;
use glib::subclass::InitializingObject;
use gtk::{glib, template_callbacks, CompositeTemplate, Entry};

#[derive(CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/fingerprint_window.ui")]
pub struct FingerprintWindow {
    // #[template_child]
    // pub fingerprint_entry: TemplateChild<Entry>,
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
    // #[template_callback]
    // fn handle_confirm() {}
}

impl ObjectImpl for FingerprintWindow {
    fn constructed(&self) {
        self.parent_constructed();
    }
}

impl WidgetImpl for FingerprintWindow {}
impl WindowImpl for FingerprintWindow {}
impl ApplicationWindowImpl for FingerprintWindow {}
impl AdwWindowImpl for FingerprintWindow {}
