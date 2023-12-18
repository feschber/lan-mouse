use crate::{
    consumer::EventConsumer,
    event::{KeyboardEvent, PointerEvent},
    scancode,
};
use anyhow::Result;
use async_trait::async_trait;
use winapi::um::winuser::{SendInput, KEYEVENTF_EXTENDEDKEY};
use winapi::{
    self,
    um::winuser::{
        INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
        LPINPUT, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
        MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT,
    },
};

use crate::{
    client::{ClientEvent, ClientHandle},
    event::Event,
};

pub struct WindowsConsumer {}

impl WindowsConsumer {
    pub fn new() -> Result<Self> {
        Ok(Self {})
    }
}

#[async_trait]
impl EventConsumer for WindowsConsumer {
    async fn consume(&mut self, event: Event, _: ClientHandle) {
        match event {
            Event::Pointer(pointer_event) => match pointer_event {
                PointerEvent::Motion {
                    time: _,
                    relative_x,
                    relative_y,
                } => {
                    rel_mouse(relative_x as i32, relative_y as i32);
                }
                PointerEvent::Button {
                    time: _,
                    button,
                    state,
                } => mouse_button(button, state),
                PointerEvent::Axis {
                    time: _,
                    axis,
                    value,
                } => scroll(axis, value),
                PointerEvent::Frame {} => {}
            },
            Event::Keyboard(keyboard_event) => match keyboard_event {
                KeyboardEvent::Key {
                    time: _,
                    key,
                    state,
                } => key_event(key, state),
                KeyboardEvent::Modifiers { .. } => {}
            },
            _ => {}
        }
    }

    async fn notify(&mut self, _: ClientEvent) {
        // nothing to do
    }

    async fn destroy(&mut self) {}
}

fn send_mouse_input(mi: MOUSEINPUT) {
    unsafe {
        let mut input = INPUT {
            type_: INPUT_MOUSE,
            u: std::mem::transmute(mi),
        };

        SendInput(
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
            _ => return,
        },
        1 => match button {
            0x110 => MOUSEEVENTF_LEFTDOWN,
            0x111 => MOUSEEVENTF_RIGHTDOWN,
            0x112 => MOUSEEVENTF_MIDDLEDOWN,
            _ => return,
        },
        _ => return,
    };
    let mi = MOUSEINPUT {
        dx: 0,
        dy: 0, // no movement
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
        _ => return,
    };
    let mi = MOUSEINPUT {
        dx: 0,
        dy: 0,
        mouseData: (-value * 15.0) as i32 as u32,
        dwFlags: event_type,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse_input(mi);
}

fn key_event(key: u32, state: u8) {
    let scancode = match linux_keycode_to_windows_scancode(key) {
        Some(code) => code,
        None => return,
    };
    let extended = scancode > 0xff;
    let scancode = scancode & 0xff;
    let ki = KEYBDINPUT {
        wVk: 0,
        wScan: scancode,
        dwFlags: KEYEVENTF_SCANCODE
            | if extended { KEYEVENTF_EXTENDEDKEY } else { 0 }
            | match state {
                0 => KEYEVENTF_KEYUP,
                1 => 0u32,
                _ => return,
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
        SendInput(1 as u32, &mut input, std::mem::size_of::<INPUT>() as i32);
    }
}

fn linux_keycode_to_windows_scancode(linux_keycode: u32) -> Option<u16> {
    let linux_scancode = match scancode::Linux::try_from(linux_keycode) {
        Ok(s) => s,
        Err(_) => {
            log::warn!("unknown keycode: {linux_keycode}");
            return None;
        }
    };
    log::trace!("linux code: {linux_scancode:?}");
    let windows_scancode = match scancode::Windows::try_from(linux_scancode) {
        Ok(s) => s,
        Err(_) => {
            log::warn!("failed to translate linux code into windows scancode: {linux_scancode:?}");
            return None;
        }
    };
    log::trace!("windows code: {windows_scancode:?}");
    Some(windows_scancode as u16)
}
