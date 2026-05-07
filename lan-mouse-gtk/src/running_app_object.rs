use std::cell::RefCell;

use glib::Object;
use gtk::{gdk, glib, subclass::prelude::*};
use lan_mouse_ipc::RunningApp;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct RunningAppObject {
        pub display_name: RefCell<String>,
        pub identifier: RefCell<String>,
        pub icon: RefCell<Option<gdk::Texture>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RunningAppObject {
        const NAME: &'static str = "LanMouseRunningAppObject";
        type Type = super::RunningAppObject;
    }

    impl ObjectImpl for RunningAppObject {}
}

glib::wrapper! {
    pub struct RunningAppObject(ObjectSubclass<imp::RunningAppObject>);
}

impl RunningAppObject {
    pub fn new(app: &RunningApp) -> Self {
        let obj: Self = Object::builder().build();
        obj.imp().display_name.replace(app.display_name.clone());
        obj.imp().identifier.replace(app.identifier.clone());
        if let Some(bytes) = &app.icon_png {
            let glib_bytes = glib::Bytes::from(bytes);
            if let Ok(texture) = gdk::Texture::from_bytes(&glib_bytes) {
                obj.imp().icon.replace(Some(texture));
            }
        }
        obj
    }

    pub fn display_name(&self) -> String {
        self.imp().display_name.borrow().clone()
    }

    pub fn identifier(&self) -> String {
        self.imp().identifier.borrow().clone()
    }

    pub fn icon(&self) -> Option<gdk::Texture> {
        self.imp().icon.borrow().clone()
    }
}
