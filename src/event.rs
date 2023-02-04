use std::{error::Error, fmt::{self, format}};

pub mod producer;
pub mod consumer;
pub mod server;

pub enum PointerEvent {
    Motion { time: u32, relative_x: f64, relative_y: f64, },
    Button { time: u32, button: u32, state: u32, },
    Axis   { time: u32, axis: u8, value: f64, },
    Frame  {}
}

pub enum KeyboardEvent {
    Key { serial: u32, time: u32, key: u32, state: u8, },
    Modifiers { serial: u32, mods_depressed: u32, mods_latched: u32, mods_locked: u32, group: u32, },
}

pub enum Event {
    Pointer(PointerEvent),
    Keyboard(KeyboardEvent),
    Release(),
}

unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    fn event_type(&self) -> EventType {
        match self {
            Self::Pointer(_) => EventType::POINTER,
            Self::Keyboard(_) => EventType::KEYBOARD,
            Self::Release() => EventType::RELEASE,
        }
    }
}

impl PointerEvent {
    fn event_type(&self) -> PointerEventType {
        match self {
            Self::Axis {..} => PointerEventType::AXIS,
            Self::Button {..} => PointerEventType::BUTTON,
            Self::Frame {..} => PointerEventType::FRAME,
            Self::Motion {..} => PointerEventType::MOTION,
        }
    }
}

enum PointerEventType { MOTION, BUTTON, AXIS, FRAME }
enum KeyboardEventType { KEY, MODIFIERS }
enum EventType { POINTER, KEYBOARD, RELEASE }


impl Into<Vec<u8>> for &Event {
    fn into(self) -> Vec<u8> {
        let event_id = vec![self.event_type() as u8];
        let event_data = match self {
            Event::Pointer(p) => p.into(),
            Event::Keyboard(k) => k.into(),
            Event::Release() => vec![],
        };
        vec![event_id, event_data].concat()
    }
}

#[derive(Debug)]
struct ProtocolError {
    msg: String,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Protocol violation: {}", self.msg)
    }
}
impl Error for ProtocolError {}

impl TryFrom<Vec<u8>> for Event {
    type Error = Box<dyn Error>;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        let event_id = u8::from_be_bytes(value[..1].try_into()?);
        match event_id >> 5 {
            i if i == (EventType::POINTER as u8) => Ok(Event::Pointer(value[1..].try_into()?)),
            i if i ==(EventType::KEYBOARD as u8) => Ok(Event::Keyboard(value[1..].try_into()?)),
            i if i == (EventType::RELEASE as u8) => Ok(Event::Release()),
            _ => Err(Box::new(ProtocolError{ msg: format!("invalid event_id {}", event_id) })),
        }
    }
}

impl Into<Vec<u8>> for &PointerEvent {
    fn into(self) -> Vec<u8> {
        let id = vec![self.event_type() as u8];
        let data = match self {
            PointerEvent::Motion { time, relative_x, relative_y } => {
                let time = time.to_be_bytes();
                let relative_x = relative_x.to_be_bytes();
                let relative_y = relative_y.to_be_bytes();
                vec![&time[..], &relative_x[..], &relative_y[..]].concat()
            },
            PointerEvent::Button { time, button, state } => {
                let time = time.to_be_bytes();
                let button = button.to_be_bytes();
                let state = state.to_be_bytes();
                vec![&time[..], &button[..], &state[..]].concat()
            },
            PointerEvent::Axis { time, axis, value } => {
                let time = time.to_be_bytes();
                let axis = axis.to_be_bytes();
                let value = value.to_be_bytes();
                vec![&time[..], &axis[..], &value[..]].concat()
            },
            PointerEvent::Frame {  } => { vec![] },
        };
        vec![id, data].concat()
    }
}

impl TryFrom<&[u8]> for PointerEvent {
    type Error = &'static str;

    fn try_from(data: &[u8]) -> Result<Self, Self::Error> {
        match data.get(0) {
            Some(id) => match id {
                0 => {
                    let time = match data.get(1..5) {
                        Some(d) => u32::from_be_bytes(d.try_into().unwrap()),
                        None => return Err("Expected 4 Bytes at index 1"),
                    };
                    let relative_x = match data.get(5..13) {
                        Some(d) => f64::from_be_bytes(d.try_into().unwrap()),
                        None => return Err("Expected 8 Bytes at index 5"),
                    };
                    let relative_y = match data.get(13..21) {
                        Some(d) => f64::from_be_bytes(d.try_into().unwrap()),
                        None => return Err("Expected 8 Bytes at index 13"),
                    };
                    Ok(Self::Motion{ time, relative_x, relative_y })
                }
            },
            None => Err("Expected an element at index 0"),
        }
    }
}

impl Into<Vec<u8>> for &KeyboardEvent {
    fn into(self) -> Vec<u8> {
        match self {
            KeyboardEvent::Key { serial, time, key, state } => {
                let serial = serial.to_be_bytes();
                let time = time.to_be_bytes();
                let key = key.to_be_bytes();
                let state = state.to_be_bytes();
                vec![&serial[..], &time[..], &key[..], &state[..]].concat()
            },
            KeyboardEvent::Modifiers { serial, mods_depressed, mods_latched, mods_locked, group } => {
                let serial = serial.to_be_bytes();
                let mods_depressed = mods_depressed.to_be_bytes();
                let mods_latched = mods_latched.to_be_bytes();
                let mods_locked = mods_locked.to_be_bytes();
                vec![&serial[..], &mods_depressed[..], &mods_latched[..], &mods_locked[..]].concat()
            },
        }
    }
}

impl TryFrom<&[u8]> for KeyboardEvent {
    type Error = TryFromSliceError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        todo!()
    }
}
