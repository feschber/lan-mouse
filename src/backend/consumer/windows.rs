use std::sync::mpsc::Receiver;

use crate::event::{KeyboardEvent, PointerEvent};
use winapi::{
    self,
    um::winuser::{INPUT, INPUT_MOUSE, LPINPUT, MOUSEEVENTF_MOVE, MOUSEINPUT,
        MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_RIGHTDOWN,
        MOUSEEVENTF_MIDDLEDOWN,
        MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_RIGHTUP,
        MOUSEEVENTF_MIDDLEUP,
        MOUSEEVENTF_WHEEL,
        MOUSEEVENTF_HWHEEL, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_SCANCODE, KEYEVENTF_KEYUP,
    },
};

use crate::{
    client::{Client, ClientHandle},
    event::Event,
};

fn send_mouse_input(mi: MOUSEINPUT) {
    unsafe {
        let mut input = INPUT {
            type_: INPUT_MOUSE,
            u: std::mem::transmute(mi),
        };

        winapi::um::winuser::SendInput(
            1 as u32,
            &mut input as LPINPUT,
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}

fn rel_mouse(dx: i32, dy: i32) {
    let mi = MOUSEINPUT {
        dx,
        dy,
        mouseData: 0,
        dwFlags: MOUSEEVENTF_MOVE,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse_input(mi);
}

fn mouse_button(button: u32, state: u32) {
    let dw_flags = match state {
        0 => match button {
            0x110 => MOUSEEVENTF_LEFTUP,
            0x111 => MOUSEEVENTF_RIGHTUP,
            0x112 => MOUSEEVENTF_MIDDLEUP,
            _ => return
        }
        1 => match button {
            0x110 => MOUSEEVENTF_LEFTDOWN,
            0x111 => MOUSEEVENTF_RIGHTDOWN,
            0x112 => MOUSEEVENTF_MIDDLEDOWN,
            _ => return
        }
        _ => return
    };
    let mi = MOUSEINPUT {
        dx: 0, dy: 0, // no movement
        mouseData: 0,
        dwFlags: dw_flags,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse_input(mi);
}

fn scroll(axis: u8, value: f64) {
    let event_type = match axis {
        0 => MOUSEEVENTF_WHEEL,
        1 => MOUSEEVENTF_HWHEEL,
        _ => return
    };
    let mi = MOUSEINPUT {
        dx: 0, dy: 0,
        mouseData: (-value * 15.0) as i32 as u32,
        dwFlags: event_type,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse_input(mi);
}

fn key_event(key: u32, state: u8) {
    let ki = KEYBDINPUT {
        wVk: 0,
        wScan: key as u16,
        dwFlags: KEYEVENTF_SCANCODE | match state {
            0 => KEYEVENTF_KEYUP,
            1 => 0u32,
            _ => return
        },
        time: 0,
        dwExtraInfo: 0,
    };
    send_keyboard_input(ki);
}

fn send_keyboard_input(ki: KEYBDINPUT) {
    unsafe {
        let mut input = INPUT {
            type_: INPUT_KEYBOARD,
            u: std::mem::zeroed(),
        };
        *input.u.ki_mut() = ki;
        winapi::um::winuser::SendInput(1 as u32, &mut input, std::mem::size_of::<INPUT>() as i32);
    }
}

pub fn run(event_rx: Receiver<(Event, ClientHandle)>, _clients: Vec<Client>) {
    loop {
        match event_rx.recv().expect("event receiver unavailable").0 {
            Event::Pointer(pointer_event) => match pointer_event {
                PointerEvent::Motion {
                    time: _,
                    relative_x,
                    relative_y,
                } => {
                    rel_mouse(relative_x as i32, relative_y as i32);
                }
                PointerEvent::Button { time:_, button, state } => { mouse_button(button, state)}
                PointerEvent::Axis { time:_, axis, value } => { scroll(axis, value) }
                PointerEvent::Frame {} => {}
            },
            Event::Keyboard(keyboard_event) => match keyboard_event {
                KeyboardEvent::Key { time:_, key, state } => { key_event(key, state) }
                KeyboardEvent::Modifiers { .. } => {}
            },
            Event::Release() => {}
        }
    }
}
