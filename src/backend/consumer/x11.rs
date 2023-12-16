use async_trait::async_trait;
use std::ptr;
use x11::{xlib, xtest};

use crate::{client::ClientHandle, consumer::EventConsumer, event::{Event, PointerEvent, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_FORWARD, BTN_BACK}};

pub struct X11Consumer {
    display: *mut xlib::Display,
}

unsafe impl Send for X11Consumer {}

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
        }
    }

    fn emulate_mouse_button(&self, button: u32, state: u32) {
        unsafe {
            let x11_button = match button {
                BTN_RIGHT => 3,
                BTN_MIDDLE => 2,
                BTN_BACK => 8,
                BTN_FORWARD => 9,
                BTN_LEFT | _ => 1,
            };
            xtest::XTestFakeButtonEvent(self.display, x11_button, state as i32, 0);
        };
    }

    const SCROLL_UP: u32 = 4;
    const SCROLL_DOWN: u32 = 5;
    const SCROLL_LEFT: u32 = 6;
    const SCROLL_RIGHT: u32 = 7;

    fn emulate_scroll(&self, axis: u8, value: f64) {
        let direction = match axis {
            1 => if value < 0.0 { Self::SCROLL_LEFT } else { Self::SCROLL_RIGHT },
            _ => if value < 0.0 { Self::SCROLL_UP } else { Self::SCROLL_DOWN },
        };

        unsafe {
            xtest::XTestFakeButtonEvent(self.display, direction, 1, 0);
            xtest::XTestFakeButtonEvent(self.display, direction, 0, 0);
        }
    }
}

impl Default for X11Consumer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventConsumer for X11Consumer {
    async fn consume(&mut self, event: Event, _: ClientHandle) {
        match event {
            Event::Pointer(pointer_event) => match pointer_event {
                PointerEvent::Motion {
                    time: _,
                    relative_x,
                    relative_y,
                } => {
                    self.relative_motion(relative_x as i32, relative_y as i32);
                }
                PointerEvent::Button { time: _, button, state } => {
                    self.emulate_mouse_button(button, state);
                }
                PointerEvent::Axis { time: _, axis, value } => {
                    self.emulate_scroll(axis, value);
                }
                PointerEvent::Frame {} => {}
            },
            Event::Keyboard(_) => {}
            _ => {}
        }
        unsafe {
            xlib::XFlush(self.display);
        }
    }

    async fn notify(&mut self, _: crate::client::ClientEvent) {
        // for our purposes it does not matter what client sent the event
    }

    async fn destroy(&mut self) {}
}
