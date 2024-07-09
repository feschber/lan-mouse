use std::{fmt::Debug, slice::SliceIndex};

use crate::ProtocolError;

use super::{Event, KeyboardEvent, PointerEvent};

enum PointerEventType {
    Motion,
    Button,
    Axis,
    AxisDiscrete120,
    Frame,
}

enum KeyboardEventType {
    Key,
    Modifiers,
}

enum EventType {
    Pointer,
    Keyboard,
    Enter,
    Leave,
    Ping,
    Pong,
    Disconnect,
}

impl Event {
    fn event_type(&self) -> EventType {
        match self {
            Self::Pointer(_) => EventType::Pointer,
            Self::Keyboard(_) => EventType::Keyboard,
            Self::Enter() => EventType::Enter,
            Self::Leave() => EventType::Leave,
            Self::Ping() => EventType::Ping,
            Self::Pong() => EventType::Pong,
            Self::Disconnect() => EventType::Disconnect,
        }
    }
}

impl PointerEvent {
    fn event_type(&self) -> PointerEventType {
        match self {
            Self::Motion { .. } => PointerEventType::Motion,
            Self::Button { .. } => PointerEventType::Button,
            Self::Axis { .. } => PointerEventType::Axis,
            Self::AxisDiscrete120 { .. } => PointerEventType::AxisDiscrete120,
            Self::Frame { .. } => PointerEventType::Frame,
        }
    }
}

impl KeyboardEvent {
    fn event_type(&self) -> KeyboardEventType {
        match self {
            KeyboardEvent::Key { .. } => KeyboardEventType::Key,
            KeyboardEvent::Modifiers { .. } => KeyboardEventType::Modifiers,
        }
    }
}

impl TryFrom<u8> for PointerEventType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, ProtocolError> {
        match value {
            x if x == Self::Motion as u8 => Ok(Self::Motion),
            x if x == Self::Button as u8 => Ok(Self::Button),
            x if x == Self::Axis as u8 => Ok(Self::Axis),
            x if x == Self::AxisDiscrete120 as u8 => Ok(Self::AxisDiscrete120),
            x if x == Self::Frame as u8 => Ok(Self::Frame),
            _ => Err(ProtocolError::InvalidPointerEventId(value)),
        }
    }
}

impl TryFrom<u8> for KeyboardEventType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, ProtocolError> {
        match value {
            x if x == Self::Key as u8 => Ok(Self::Key),
            x if x == Self::Modifiers as u8 => Ok(Self::Modifiers),
            _ => Err(ProtocolError::InvalidKeyboardEventId(value)),
        }
    }
}

impl From<&Event> for Vec<u8> {
    fn from(event: &Event) -> Self {
        let event_id = vec![event.event_type() as u8];
        let event_data = match event {
            Event::Pointer(p) => p.into(),
            Event::Keyboard(k) => k.into(),
            Event::Enter() => vec![],
            Event::Leave() => vec![],
            Event::Ping() => vec![],
            Event::Pong() => vec![],
            Event::Disconnect() => vec![],
        };
        [event_id, event_data].concat()
    }
}

impl TryFrom<Vec<u8>> for Event {
    type Error = ProtocolError;

    fn try_from(value: Vec<u8>) -> Result<Self, ProtocolError> {
        let event_id = u8::from_be_bytes(value[..1].try_into()?);
        match event_id {
            i if i == (EventType::Pointer as u8) => Ok(Event::Pointer(value.try_into()?)),
            i if i == (EventType::Keyboard as u8) => Ok(Event::Keyboard(value.try_into()?)),
            i if i == (EventType::Enter as u8) => Ok(Event::Enter()),
            i if i == (EventType::Leave as u8) => Ok(Event::Leave()),
            i if i == (EventType::Ping as u8) => Ok(Event::Ping()),
            i if i == (EventType::Pong as u8) => Ok(Event::Pong()),
            i if i == (EventType::Disconnect as u8) => Ok(Event::Disconnect()),
            _ => Err(ProtocolError::InvalidEventId(event_id)),
        }
    }
}

impl From<&PointerEvent> for Vec<u8> {
    fn from(event: &PointerEvent) -> Self {
        let id = vec![event.event_type() as u8];
        let data = match event {
            PointerEvent::Motion {
                time,
                dx: relative_x,
                dy: relative_y,
            } => {
                let time = time.to_be_bytes();
                let relative_x = relative_x.to_be_bytes();
                let relative_y = relative_y.to_be_bytes();
                [&time[..], &relative_x[..], &relative_y[..]].concat()
            }
            PointerEvent::Button {
                time,
                button,
                state,
            } => {
                let time = time.to_be_bytes();
                let button = button.to_be_bytes();
                let state = state.to_be_bytes();
                [&time[..], &button[..], &state[..]].concat()
            }
            PointerEvent::Axis { time, axis, value } => {
                let time = time.to_be_bytes();
                let axis = axis.to_be_bytes();
                let value = value.to_be_bytes();
                [&time[..], &axis[..], &value[..]].concat()
            }
            PointerEvent::AxisDiscrete120 { axis, value } => {
                let axis = axis.to_be_bytes();
                let value = value.to_be_bytes();
                [&axis[..], &value[..]].concat()
            }
            PointerEvent::Frame {} => {
                vec![]
            }
        };
        [id, data].concat()
    }
}

fn decode_u8<I>(data: &[u8], idx: I) -> Result<u8, ProtocolError>
where
    I: SliceIndex<[u8], Output = [u8]> + Debug + Clone,
{
    let data = data
        .get(idx.clone())
        .ok_or(ProtocolError::Data(format!("{:?}", idx)))?;
    Ok(u8::from_be_bytes(data.try_into()?))
}

fn decode_u32<I>(data: &[u8], idx: I) -> Result<u32, ProtocolError>
where
    I: SliceIndex<[u8], Output = [u8]> + Debug + Clone,
{
    let data = data
        .get(idx.clone())
        .ok_or(ProtocolError::Data(format!("{:?}", idx)))?;
    Ok(u32::from_be_bytes(data.try_into()?))
}

fn decode_i32<I>(data: &[u8], idx: I) -> Result<i32, ProtocolError>
where
    I: SliceIndex<[u8], Output = [u8]> + Debug + Clone,
{
    let data = data
        .get(idx.clone())
        .ok_or(ProtocolError::Data(format!("{:?}", idx)))?;
    Ok(i32::from_be_bytes(data.try_into()?))
}
fn decode_f64<I>(data: &[u8], idx: I) -> Result<f64, ProtocolError>
where
    I: SliceIndex<[u8], Output = [u8]> + Debug + Clone,
{
    let data = data
        .get(idx.clone())
        .ok_or(ProtocolError::Data(format!("{:?}", idx)))?;
    Ok(f64::from_be_bytes(data.try_into()?))
}

impl TryFrom<Vec<u8>> for PointerEvent {
    type Error = ProtocolError;

    fn try_from(data: Vec<u8>) -> Result<Self, ProtocolError> {
        match data.get(1) {
            Some(id) => match id.to_owned().try_into()? {
                PointerEventType::Motion => {
                    let time = decode_u32(&data, 2..6)?;
                    let dx = decode_f64(&data, 6..14)?;
                    let dy = decode_f64(&data, 14..22)?;

                    Ok(Self::Motion { time, dx, dy })
                }
                PointerEventType::Button => {
                    let time = decode_u32(&data, 2..6)?;
                    let button = decode_u32(&data, 6..10)?;
                    let state = decode_u32(&data, 10..14)?;

                    Ok(Self::Button {
                        time,
                        button,
                        state,
                    })
                }
                PointerEventType::Axis => {
                    let time = decode_u32(&data, 2..6)?;
                    let axis = decode_u8(&data, 6..7)?;
                    let value = decode_f64(&data, 7..15)?;
                    Ok(Self::Axis { time, axis, value })
                }
                PointerEventType::AxisDiscrete120 => {
                    let axis = decode_u8(&data, 2..3)?;
                    let value = decode_i32(&data, 3..7)?;
                    Ok(Self::AxisDiscrete120 { axis, value })
                }
                PointerEventType::Frame => Ok(Self::Frame {}),
            },
            None => Err(ProtocolError::Data("0".to_string())),
        }
    }
}

impl From<&KeyboardEvent> for Vec<u8> {
    fn from(event: &KeyboardEvent) -> Self {
        let id = vec![event.event_type() as u8];
        let data = match event {
            KeyboardEvent::Key { time, key, state } => {
                let time = time.to_be_bytes();
                let key = key.to_be_bytes();
                let state = state.to_be_bytes();
                [&time[..], &key[..], &state[..]].concat()
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
                [
                    &mods_depressed[..],
                    &mods_latched[..],
                    &mods_locked[..],
                    &group[..],
                ]
                .concat()
            }
        };
        [id, data].concat()
    }
}

impl TryFrom<Vec<u8>> for KeyboardEvent {
    type Error = ProtocolError;

    fn try_from(data: Vec<u8>) -> Result<Self, ProtocolError> {
        match data.get(1) {
            Some(id) => match id.to_owned().try_into()? {
                KeyboardEventType::Key => {
                    let time = decode_u32(&data, 2..6)?;
                    let key = decode_u32(&data, 6..10)?;
                    let state = decode_u8(&data, 10..11)?;
                    Ok(KeyboardEvent::Key { time, key, state })
                }
                KeyboardEventType::Modifiers => {
                    let mods_depressed = decode_u32(&data, 2..6)?;
                    let mods_latched = decode_u32(&data, 6..10)?;
                    let mods_locked = decode_u32(&data, 10..14)?;
                    let group = decode_u32(&data, 14..18)?;
                    Ok(KeyboardEvent::Modifiers {
                        mods_depressed,
                        mods_latched,
                        mods_locked,
                        group,
                    })
                }
            },
            None => Err(ProtocolError::Data("0".to_string())),
        }
    }
}
