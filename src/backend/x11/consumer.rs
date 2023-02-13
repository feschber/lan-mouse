use std::{sync::mpsc::Receiver, ptr};
use x11::{xtest, xlib};

use crate::{client::{ClientHandle, Client}, event::Event};

fn open_display() -> Option<*mut xlib::Display> {
    unsafe {
        match xlib::XOpenDisplay(ptr::null()) {
            d if d == ptr::null::<xlib::Display>() as *mut xlib::Display => None,
            display => Some(display),
        }
    }
}

fn relative_motion(display: *mut xlib::Display, dx: i32, dy: i32) {
    unsafe {
        xtest::XTestFakeRelativeMotionEvent(display, dx, dy, 0, 0);
        xlib::XFlush(display);
    }
}

pub fn run(event_rx: Receiver<(Event, ClientHandle)>, _clients: Vec<Client>) {
    let display = match open_display() {
        None => panic!("could not open display!"),
        Some(display) => display,
    };

    loop {
        match event_rx.recv().expect("event receiver unavailable").0 {
            Event::Pointer(pointer_event) => {
                match pointer_event {
                    crate::event::PointerEvent::Motion { time: _, relative_x, relative_y } => {
                        relative_motion(display, relative_x as i32, relative_y as i32);
                    },
                    crate::event::PointerEvent::Button { .. } => {},
                    crate::event::PointerEvent::Axis { .. } => {},
                    crate::event::PointerEvent::Frame {  } => {},
                }
            },
            Event::Keyboard(_) => {},
            Event::Release() => {},
        }
    }
}
