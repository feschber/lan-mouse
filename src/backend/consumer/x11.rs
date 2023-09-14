use std::ptr;
use x11::{xlib, xtest};

use crate::{
    client::ClientHandle,
    event::Event, consumer::Consumer,
};

pub struct X11Consumer {
    display: *mut xlib::Display,
}

impl X11Consumer {
    pub fn new() -> Self {
        let display = unsafe {
            match xlib::XOpenDisplay(ptr::null()) {
                d if d == ptr::null::<xlib::Display>() as *mut xlib::Display => None,
                display => Some(display),
            }
        };
        let display = display.expect("could not open display");
        Self { display }
    }

    fn relative_motion(&self, dx: i32, dy: i32) {
        unsafe {
            xtest::XTestFakeRelativeMotionEvent(self.display, dx, dy, 0, 0);
            xlib::XFlush(self.display);
        }
    }
}

impl Consumer for X11Consumer {
    fn consume(&self, event: Event, _: ClientHandle) {
        match event {
            Event::Pointer(pointer_event) => match pointer_event {
                crate::event::PointerEvent::Motion {
                    time: _,
                    relative_x,
                    relative_y,
                } => {
                    self.relative_motion(relative_x as i32, relative_y as i32);
                }
                crate::event::PointerEvent::Button { .. } => {}
                crate::event::PointerEvent::Axis { .. } => {}
                crate::event::PointerEvent::Frame {} => {}
            },
            Event::Keyboard(_) => {}
            Event::Release() => {}
        }
    }

    fn notify(&mut self, _: crate::client::ClientEvent) {
        // for our purposes it does not matter what client sent the event
    }
}

