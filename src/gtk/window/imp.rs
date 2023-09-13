use std::cell::{Cell, RefCell};

use glib::subclass::InitializingObject;
use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{glib, Button, CompositeTemplate, ListBox, gio};

// ANCHOR: object
// Object holding the state
#[derive(CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/window.ui")]
pub struct Window {
    pub number: Cell<i32>,
    #[template_child]
    pub add_client_button: TemplateChild<Button>,
    #[template_child]
    pub client_list: TemplateChild<ListBox>,
    pub clients: RefCell<Option<gio::ListStore>>,
}
// ANCHOR_END: object

// ANCHOR: subclass
// The central trait for subclassing a GObject
#[glib::object_subclass]
impl ObjectSubclass for Window {
    // `NAME` needs to match `class` attribute of template
    const NAME: &'static str = "LanMouseWindow";
    type Type = super::Window;
    type ParentType = gtk::ApplicationWindow;

    fn class_init(klass: &mut Self::Class) {
        klass.bind_template();
        klass.bind_template_callbacks();
    }

    fn instance_init(obj: &InitializingObject<Self>) {
        obj.init_template();
    }
}
// ANCHOR_END: subclass

// ANCHOR: template_callbacks
#[gtk::template_callbacks]
impl Window {
    #[template_callback]
    fn handle_button_clicked(&self, button: &Button) {
        let number_increased = self.number.get() + 1;
        self.number.set(number_increased);
        button.set_label(&number_increased.to_string())
    }

    #[template_callback]
    fn handle_add_client(&self, _button: &Button) {
        println!("Add client");
    }

    // fn create_client_row(&self, client_object: &ClientObject) -> ActionRow {

    // }
}
// ANCHOR_END: template_callbacks

// Trait shared by all GObjects
impl ObjectImpl for Window {
    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        obj.setup_icon();
        obj.setup_clients();
        obj.setup_callbacks();
    }
}

// Trait shared by all widgets
impl WidgetImpl for Window {}

// Trait shared by all windows
impl WindowImpl for Window {}

// Trait shared by all application windows
impl ApplicationWindowImpl for Window {}
