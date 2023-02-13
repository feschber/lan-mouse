use std::{error::Error, fmt};

pub mod server;

pub enum PointerEvent {
    Motion {
        time: u32,
        relative_x: f64,
        relative_y: f64,
    },
    Button {
        time: u32,
        button: u32,
        state: u32,
    },
    Axis {
        time: u32,
        axis: u8,
        value: f64,
    },
    Frame {},
}

pub enum KeyboardEvent {
    Key {
        time: u32,
        key: u32,
        state: u8,
    },
    Modifiers {
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        group: u32,
    },
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
            Self::Motion { .. } => PointerEventType::MOTION,
            Self::Button { .. } => PointerEventType::BUTTON,
            Self::Axis { .. } => PointerEventType::AXIS,
            Self::Frame { .. } => PointerEventType::FRAME,
        }
    }
}

impl KeyboardEvent {
    fn event_type(&self) -> KeyboardEventType {
        match self {
            KeyboardEvent::Key { .. } => KeyboardEventType::KEY,
            KeyboardEvent::Modifiers { .. } => KeyboardEventType::MODIFIERS,
        }
    }
}

enum PointerEventType {
    MOTION,
    BUTTON,
    AXIS,
    FRAME,
}
enum KeyboardEventType {
    KEY,
    MODIFIERS,
}
enum EventType {
    POINTER,
    KEYBOARD,
    RELEASE,
}

impl TryFrom<u8> for PointerEventType {
    type Error = Box<dyn Error>;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            x if x == Self::MOTION as u8 => Ok(Self::MOTION),
            x if x == Self::BUTTON as u8 => Ok(Self::BUTTON),
            x if x == Self::AXIS as u8 => Ok(Self::AXIS),
            x if x == Self::FRAME as u8 => Ok(Self::FRAME),
            _ => Err(Box::new(ProtocolError {
                msg: format!("invalid pointer event type {}", value),
            })),
        }
    }
}

impl TryFrom<u8> for KeyboardEventType {
    type Error = Box<dyn Error>;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            x if x == Self::KEY as u8 => Ok(Self::KEY),
            x if x == Self::MODIFIERS as u8 => Ok(Self::MODIFIERS),
            _ => Err(Box::new(ProtocolError {
                msg: format!("invalid keyboard event type {}", value),
            })),
        }
    }
}

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
        match event_id {
            i if i == (EventType::POINTER as u8) => Ok(Event::Pointer(value.try_into()?)),
            i if i == (EventType::KEYBOARD as u8) => Ok(Event::Keyboard(value.try_into()?)),
            i if i == (EventType::RELEASE as u8) => Ok(Event::Release()),
            _ => Err(Box::new(ProtocolError {
                msg: format!("invalid event_id {}", event_id),
            })),
        }
    }
}

impl Into<Vec<u8>> for &PointerEvent {
    fn into(self) -> Vec<u8> {
        let id = vec![self.event_type() as u8];
        let data = match self {
            PointerEvent::Motion {
                time,
                relative_x,
                relative_y,
            } => {
                let time = time.to_be_bytes();
                let relative_x = relative_x.to_be_bytes();
                let relative_y = relative_y.to_be_bytes();
                vec![&time[..], &relative_x[..], &relative_y[..]].concat()
            }
            PointerEvent::Button {
                time,
                button,
                state,
            } => {
                let time = time.to_be_bytes();
                let button = button.to_be_bytes();
                let state = state.to_be_bytes();
                vec![&time[..], &button[..], &state[..]].concat()
            }
            PointerEvent::Axis { time, axis, value } => {
                let time = time.to_be_bytes();
                let axis = axis.to_be_bytes();
                let value = value.to_be_bytes();
                vec![&time[..], &axis[..], &value[..]].concat()
            }
            PointerEvent::Frame {} => {
                vec![]
            }
        };
        vec![id, data].concat()
    }
}

impl TryFrom<Vec<u8>> for PointerEvent {
    type Error = Box<dyn Error>;

    fn try_from(data: Vec<u8>) -> Result<Self, Self::Error> {
        match data.get(1) {
            Some(id) => {
                let event_type = match id.to_owned().try_into() {
                    Ok(event_type) => event_type,
                    Err(e) => return Err(e),
                };
                match event_type {
                    PointerEventType::MOTION => {
                        let time = match data.get(2..6) {
                            Some(d) => u32::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 4 Bytes at index 2".into(),
                                }))
                            }
                        };
                        let relative_x = match data.get(6..14) {
                            Some(d) => f64::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 8 Bytes at index 6".into(),
                                }))
                            }
                        };
                        let relative_y = match data.get(14..22) {
                            Some(d) => f64::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 8 Bytes at index 14".into(),
                                }))
                            }
                        };
                        Ok(Self::Motion {
                            time,
                            relative_x,
                            relative_y,
                        })
                    }
                    PointerEventType::BUTTON => {
                        let time = match data.get(2..6) {
                            Some(d) => u32::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 4 Bytes at index 2".into(),
                                }))
                            }
                        };
                        let button = match data.get(6..10) {
                            Some(d) => u32::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 4 Bytes at index 10".into(),
                                }))
                            }
                        };
                        let state = match data.get(10..14) {
                            Some(d) => u32::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 4 Bytes at index 14".into(),
                                }))
                            }
                        };
                        Ok(Self::Button {
                            time,
                            button,
                            state,
                        })
                    }
                    PointerEventType::AXIS => {
                        let time = match data.get(2..6) {
                            Some(d) => u32::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 4 Bytes at index 2".into(),
                                }))
                            }
                        };
                        let axis = match data.get(6) {
                            Some(d) => *d,
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 1 Byte at index 6".into(),
                                }));
                            }
                        };
                        let value = match data.get(7..15) {
                            Some(d) => f64::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 8 Bytes at index 7".into(),
                                }));
                            }
                        };
                        Ok(Self::Axis { time, axis, value })
                    }
                    PointerEventType::FRAME => Ok(Self::Frame {}),
                }
            }
            None => Err(Box::new(ProtocolError {
                msg: "Expected an element at index 0".into(),
            })),
        }
    }
}

impl Into<Vec<u8>> for &KeyboardEvent {
    fn into(self) -> Vec<u8> {
        let id = vec![self.event_type() as u8];
        let data = match self {
            KeyboardEvent::Key { time, key, state } => {
                let time = time.to_be_bytes();
                let key = key.to_be_bytes();
                let state = state.to_be_bytes();
                vec![&time[..], &key[..], &state[..]].concat()
            }
            KeyboardEvent::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                let mods_depressed = mods_depressed.to_be_bytes();
                let mods_latched = mods_latched.to_be_bytes();
                let mods_locked = mods_locked.to_be_bytes();
                let group = group.to_be_bytes();
                vec![
                    &mods_depressed[..],
                    &mods_latched[..],
                    &mods_locked[..],
                    &group[..],
                ]
                .concat()
            }
        };
        vec![id, data].concat()
    }
}

impl TryFrom<Vec<u8>> for KeyboardEvent {
    type Error = Box<dyn Error>;

    fn try_from(data: Vec<u8>) -> Result<Self, Self::Error> {
        match data.get(1) {
            Some(id) => {
                let event_type = match id.to_owned().try_into() {
                    Ok(event_type) => event_type,
                    Err(e) => return Err(e),
                };
                match event_type {
                    KeyboardEventType::KEY => {
                        let time = match data.get(2..6) {
                            Some(d) => u32::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 4 Bytes at index 6".into(),
                                }))
                            }
                        };
                        let key = match data.get(6..10) {
                            Some(d) => u32::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 4 Bytes at index 10".into(),
                                }))
                            }
                        };
                        let state = match data.get(10) {
                            Some(d) => *d,
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 1 Bytes at index 14".into(),
                                }))
                            }
                        };
                        Ok(KeyboardEvent::Key { time, key, state })
                    }
                    KeyboardEventType::MODIFIERS => {
                        let mods_depressed = match data.get(2..6) {
                            Some(d) => u32::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 4 Bytes at index 6".into(),
                                }))
                            }
                        };
                        let mods_latched = match data.get(6..10) {
                            Some(d) => u32::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 4 Bytes at index 10".into(),
                                }))
                            }
                        };
                        let mods_locked = match data.get(10..14) {
                            Some(d) => u32::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 4 Bytes at index 14".into(),
                                }))
                            }
                        };
                        let group = match data.get(14..18) {
                            Some(d) => u32::from_be_bytes(d.try_into().unwrap()),
                            None => {
                                return Err(Box::new(ProtocolError {
                                    msg: "Expected 4 Bytes at index 18".into(),
                                }))
                            }
                        };
                        Ok(KeyboardEvent::Modifiers {
                            mods_depressed,
                            mods_latched,
                            mods_locked,
                            group,
                        })
                    }
                }
            }
            None => Err(Box::new(ProtocolError {
                msg: "Expected an element at index 0".into(),
            })),
        }
    }
}
