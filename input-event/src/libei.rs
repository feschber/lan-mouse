use reis::{
    ei::{button::ButtonState, keyboard::KeyState},
    event::EiEvent,
};

use crate::{Event, KeyboardEvent, PointerEvent};

impl Event {
    pub fn from_ei_event(ei_event: EiEvent) -> impl Iterator<Item = Self> {
        to_input_events(ei_event).into_iter()
    }
}

enum Events {
    None,
    One(Event),
    Two(Event, Event),
}

impl Events {
    fn into_iter(self) -> impl Iterator<Item = Event> {
        EventIterator::new(self)
    }
}

struct EventIterator {
    events: [Option<Event>; 2],
    pos: usize,
}

impl EventIterator {
    fn new(events: Events) -> Self {
        let events = match events {
            Events::None => [None, None],
            Events::One(e) => [Some(e), None],
            Events::Two(e, f) => [Some(e), Some(f)],
        };
        Self { events, pos: 0 }
    }
}

impl Iterator for EventIterator {
    type Item = Event;

    fn next(&mut self) -> Option<Self::Item> {
        let res = if self.pos >= self.events.len() {
            None
        } else {
            self.events[self.pos]
        };
        self.pos += 1;
        res
    }
}

fn to_input_events(ei_event: EiEvent) -> Events {
    match ei_event {
        EiEvent::KeyboardModifiers(mods) => {
            let modifier_event = KeyboardEvent::Modifiers {
                depressed: mods.depressed,
                latched: mods.latched,
                locked: mods.locked,
                group: mods.group,
            };
            Events::One(Event::Keyboard(modifier_event))
        }
        EiEvent::Frame(_) => Events::None, /* FIXME */
        EiEvent::PointerMotion(motion) => {
            let motion_event = PointerEvent::Motion {
                time: motion.time as u32,
                dx: motion.dx as f64,
                dy: motion.dy as f64,
            };
            Events::One(Event::Pointer(motion_event))
        }
        EiEvent::PointerMotionAbsolute(_) => Events::None,
        EiEvent::Button(button) => {
            let button_event = PointerEvent::Button {
                time: button.time as u32,
                button: button.button,
                state: match button.state {
                    ButtonState::Released => 0,
                    ButtonState::Press => 1,
                },
            };
            Events::One(Event::Pointer(button_event))
        }
        EiEvent::ScrollDelta(delta) => {
            let dy = Event::Pointer(PointerEvent::Axis {
                time: 0,
                axis: 0,
                value: delta.dy as f64,
            });
            let dx = Event::Pointer(PointerEvent::Axis {
                time: 0,
                axis: 1,
                value: delta.dx as f64,
            });
            if delta.dy != 0. && delta.dx != 0. {
                Events::Two(dy, dx)
            } else if delta.dy != 0. {
                Events::One(dy)
            } else if delta.dx != 0. {
                Events::One(dx)
            } else {
                Events::None
            }
        }
        EiEvent::ScrollStop(_) => Events::None,   /* TODO */
        EiEvent::ScrollCancel(_) => Events::None, /* TODO */
        EiEvent::ScrollDiscrete(scroll) => {
            let dy = Event::Pointer(PointerEvent::AxisDiscrete120 {
                axis: 0,
                value: scroll.discrete_dy,
            });
            let dx = Event::Pointer(PointerEvent::AxisDiscrete120 {
                axis: 1,
                value: scroll.discrete_dx,
            });
            if scroll.discrete_dy != 0 && scroll.discrete_dx != 0 {
                Events::Two(dy, dx)
            } else if scroll.discrete_dy != 0 {
                Events::One(dy)
            } else if scroll.discrete_dx != 0 {
                Events::One(dx)
            } else {
                Events::None
            }
        }
        EiEvent::KeyboardKey(key) => {
            let key_event = KeyboardEvent::Key {
                key: key.key,
                state: match key.state {
                    KeyState::Press => 1,
                    KeyState::Released => 0,
                },
                time: key.time as u32,
            };
            Events::One(Event::Keyboard(key_event))
        }
        EiEvent::TouchDown(_) => Events::None,   /* TODO */
        EiEvent::TouchUp(_) => Events::None,     /* TODO */
        EiEvent::TouchMotion(_) => Events::None, /* TODO */
        _ => Events::None,
    }
}
