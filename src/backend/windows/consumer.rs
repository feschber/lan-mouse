#![cfg(windows)]
use std::sync::mpsc::Receiver;

use winapi::{self, um::winuser::{INPUT, LPINPUT, INPUT_MOUSE, MOUSEINPUT, MOUSEEVENTF_MOVE}};
use crate::event::{PointerEvent, KeyboardEvent};

use crate::{event::Event, client::{Client, ClientHandle}};

fn rel_mouse(dx: i32, dy: i32) {
    let mi = MOUSEINPUT {
        dx,
        dy,
        mouseData: 0,
        dwFlags: MOUSEEVENTF_MOVE,
        time: 0,
        dwExtraInfo: 0,
    };

    unsafe {
        let mut input = INPUT {
            type_: INPUT_MOUSE,
            u: std::mem::transmute(mi),
        };

        winapi::um::winuser::SendInput(1 as u32, &mut input as LPINPUT, std::mem::size_of::<INPUT>() as i32);
    }
}


pub fn run(event_rx: Receiver<(Event, ClientHandle)>, _clients: Vec<Client>) {
    loop {
        match event_rx.recv().expect("event receiver unavailable").0 {
            Event::Pointer(pointer_event) => {
                match pointer_event {
                    PointerEvent::Motion { time: _, relative_x, relative_y } => {
                        rel_mouse(relative_x as i32, relative_y as i32);
                    },
                    PointerEvent::Button { .. } => {},
                    PointerEvent::Axis { .. } => {},
                    PointerEvent::Frame {  } => {},
                }
            },
            Event::Keyboard(keyboard_event) => {
                match keyboard_event {
                    KeyboardEvent::Key {..} => {},
                    KeyboardEvent::Modifiers {..} => {},
                }
            },
            Event::Release() => { },
        }
    }
}
