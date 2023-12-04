use std::ops::{Index, IndexMut};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use crate::client::{ClientEvent, ClientHandle};
use crate::consumer::EventConsumer;
use crate::event::{Event, KeyboardEvent, PointerEvent};
use core_graphics::display::{CGPoint};
use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton, ScrollEventUnit};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

pub struct MacOSConsumer {
    pub event_source: CGEventSource,
    button_state: ButtonState,
}

struct ButtonState {
    left: bool,
    right: bool,
    center: bool,
}

impl Index<CGMouseButton> for ButtonState {
    type Output = bool;

    fn index(&self, index: CGMouseButton) -> &Self::Output {
        match index {
            CGMouseButton::Left => &self.left,
            CGMouseButton::Right => &self.right,
            CGMouseButton::Center => &self.center
        }
    }
}

impl IndexMut<CGMouseButton> for ButtonState {
    fn index_mut(&mut self, index: CGMouseButton) -> &mut Self::Output {
        match index {
            CGMouseButton::Left => &mut self.left,
            CGMouseButton::Right => &mut self.right,
            CGMouseButton::Center => &mut self.center
        }
    }
}

unsafe impl Send for MacOSConsumer {}

impl MacOSConsumer {
    pub fn new() -> Result<Self> {
        let event_source = match CGEventSource::new(CGEventSourceStateID::CombinedSessionState) {
            Ok(e) => e,
            Err(_) => return Err(anyhow!("event source creation failed!")),
        };
        let button_state = ButtonState { left: false, right: false, center: false };
        Ok(Self { event_source, button_state })
    }

    fn get_mouse_location(&self) -> Option<CGPoint> {
        let event: CGEvent = CGEvent::new(self.event_source.clone()).ok()?;
        Some(event.location())
    }
}

#[async_trait]
impl EventConsumer for MacOSConsumer {
    async fn consume(&mut self, event: Event, _client_handle: ClientHandle) {
        match event {
            Event::Pointer(pointer_event) => match pointer_event {
                PointerEvent::Motion { time: _, relative_x, relative_y } => {
                    let mut mouse_location = match self.get_mouse_location() {
                        Some(l) => l,
                        None => {
                            log::warn!("could not get mouse location!");
                            return
                        }
                    };
                    mouse_location.x += relative_x;
                    mouse_location.y += relative_y;

                    let mut event_type = CGEventType::MouseMoved;
                    if self.button_state.left {
                        event_type = CGEventType::LeftMouseDragged
                    } else if self.button_state.right {
                        event_type = CGEventType::RightMouseDragged
                    } else if self.button_state.center {
                        event_type = CGEventType::OtherMouseDragged
                    };
                    let event = match CGEvent::new_mouse_event(
                        self.event_source.clone(),
                        event_type,
                        mouse_location,
                        CGMouseButton::Left,
                    ) {
                        Ok(e) => e,
                        Err(_) => {
                            log::warn!("mouse event creation failed!");
                            return;
                        }
                    };
                    event.post(CGEventTapLocation::HID);
                }
                PointerEvent::Button { time: _, button, state } => {
                    let (event_type, mouse_button) = match (button, state) {
                        (b, 1) if b == crate::event::BTN_LEFT => (CGEventType::LeftMouseDown, CGMouseButton::Left),
                        (b, 0) if b == crate::event::BTN_LEFT => (CGEventType::LeftMouseUp, CGMouseButton::Left),
                        (b, 1) if b == crate::event::BTN_RIGHT => (CGEventType::RightMouseDown, CGMouseButton::Right),
                        (b, 0) if b == crate::event::BTN_RIGHT => (CGEventType::RightMouseUp, CGMouseButton::Right),
                        (b, 1) if b == crate::event::BTN_MIDDLE => (CGEventType::OtherMouseDown, CGMouseButton::Center),
                        (b, 0) if b == crate::event::BTN_MIDDLE => (CGEventType::OtherMouseUp, CGMouseButton::Center),
                        _ => {
                            log::warn!("invalid button event: {button},{state}");
                            return
                        }
                    };
                    // store button state
                    self.button_state[mouse_button] = if state == 1 { true } else { false };

                    let location = self.get_mouse_location().unwrap();
                    let event = match CGEvent::new_mouse_event(self.event_source.clone(), event_type, location, mouse_button) {
                        Ok(e) => e,
                        Err(()) => {
                            log::warn!("mouse event creation failed!");
                            return
                        }
                    };
                    event.post(CGEventTapLocation::HID);
                }
                PointerEvent::Axis { time: _, axis, value } => {
                    let value = value as i32 / 10; // FIXME: high precision scroll events
                    let (count, wheel1, wheel2, wheel3) = match axis {
                        0 => (1, value, 0, 0), // 0 = vertical => 1 scroll wheel device (y axis)
                        1 => (2, 0, value, 0), // 1 = horizontal => 2 scroll wheel devices (y, x) -> (0, x)
                        _ => {
                            log::warn!("invalid scroll event: {axis}, {value}");
                            return
                        }
                    };
                    let event = match CGEvent::new_scroll_event(self.event_source.clone(), ScrollEventUnit::LINE, count, wheel1, wheel2, wheel3) {
                        Ok(e) => e,
                        Err(()) => {
                            log::warn!("scroll event creation failed!");
                            return
                        }
                    };
                    event.post(CGEventTapLocation::HID);
                }
                PointerEvent::Frame { .. } => {}
            }
            Event::Keyboard(keyboard_event) => match keyboard_event {
                KeyboardEvent::Key { time: _, key, state } => {
                    let code = CGKeyCode::from_le(key as u16); // FIXME byteorder
                    let event = match CGEvent::new_keyboard_event(
                        self.event_source.clone(),
                        code,
                        match state { 1 => true, _ => false }
                    ) {
                        Ok(e) => e,
                        Err(_) => {
                            log::warn!("unable to create key event");
                            return
                        }
                    };
                    event.post(CGEventTapLocation::HID);
                }
                KeyboardEvent::Modifiers { .. } => {}
            }
            Event::Release() => {}
            Event::Ping() => {}
            Event::Pong() => {}
        }
    }

    async fn notify(&mut self, _client_event: ClientEvent) { }

    async fn destroy(&mut self) { }
}