use std::{cell::{Cell, RefCell}, path::PathBuf};

use glib::subclass::InitializingObject;
use adw::{prelude::*, ActionRow};
use adw::subclass::prelude::*;
use gtk::{glib, Button, CompositeTemplate, ListBox, gio};

#[derive(CompositeTemplate, Default)]
#[template(resource = "/de/feschber/LanMouse/window.ui")]
pub struct Window {
    pub number: Cell<i32>,
    #[template_child]
    pub add_client_button: TemplateChild<Button>,
    #[template_child]
    pub client_list: TemplateChild<ListBox>,
    #[template_child]
    pub client_placeholder: TemplateChild<ActionRow>,
    pub clients: RefCell<Option<gio::ListStore>>,
    pub socket_path: RefCell<Option<PathBuf>>,
}

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

#[gtk::template_callbacks]
impl Window {
    #[template_callback]
    fn handle_button_clicked(&self, button: &Button) {
        let number_increased = self.number.get() + 1;
        self.number.set(number_increased);
        button.set_label(&number_increased.to_string())
    }
}


impl ObjectImpl for Window {
    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        obj.setup_icon();
        obj.setup_clients();
        obj.setup_callbacks();
        obj.connect_stream();
    }
}

impl WidgetImpl for Window {}
impl WindowImpl for Window {}
impl ApplicationWindowImpl for Window {}
