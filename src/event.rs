pub mod producer;
pub mod consumer;
pub mod server;

/*
 * TODO: currently the wayland events are encoded
 * directly with no generalized event format
*/
use wayland_client::{protocol::{wl_pointer, wl_keyboard}, WEnum};

pub trait Encode {
    fn encode(&self) -> Vec<u8>;
}

pub trait Decode {
    fn decode(buf: Vec<u8>) -> Self;
}

impl Encode for wl_pointer::Event {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match *self {
            Self::Motion {
                time: t,
                surface_x: x,
                surface_y: y,
            } => {
                buf.push(0u8);
                buf.extend_from_slice(t.to_ne_bytes().as_ref());
                buf.extend_from_slice(x.to_ne_bytes().as_ref());
                buf.extend_from_slice(y.to_ne_bytes().as_ref());
            }
            Self::Button {
                serial: _,
                time: t,
                button: b,
                state: s,
            } => {
                buf.push(1u8);
                buf.extend_from_slice(t.to_ne_bytes().as_ref());
                buf.extend_from_slice(b.to_ne_bytes().as_ref());
                buf.push(u32::from(s) as u8);
            }
            Self::Axis {
                time: t,
                axis: a,
                value: v,
            } => {
                buf.push(2u8);
                buf.extend_from_slice(t.to_ne_bytes().as_ref());
                buf.push(u32::from(a) as u8);
                buf.extend_from_slice(v.to_ne_bytes().as_ref());
            }
            Self::Frame {} => {
                buf.push(3u8);
            }
            _ => todo!(),
        }
        buf
    }
}

impl Encode for wl_keyboard::Event {
    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        match self {
            Self::Key {
                serial: _,
                time: t,
                key: k,
                state: s,
            } => {
                buf.push(4u8);
                buf.extend_from_slice(t.to_ne_bytes().as_ref());
                buf.extend_from_slice(k.to_ne_bytes().as_ref());
                buf.push(u32::from(*s) as u8);
            }
            Self::Modifiers {
                serial: _,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                buf.push(5u8);
                buf.extend_from_slice(mods_depressed.to_ne_bytes().as_ref());
                buf.extend_from_slice(mods_latched.to_ne_bytes().as_ref());
                buf.extend_from_slice(mods_locked.to_ne_bytes().as_ref());
                buf.extend_from_slice(group.to_ne_bytes().as_ref());
            }
            _ => todo!(),
        }
        buf
    }
}

pub enum Event {
    Pointer(wl_pointer::Event),
    Keyboard(wl_keyboard::Event),
    Release(),
}

impl Encode for Event {
    fn encode(&self) -> Vec<u8> {
        match self {
            Event::Pointer(p) => p.encode(),
            Event::Keyboard(k) => k.encode(),
            Event::Release() => vec![6u8],
        }
    }
}

unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Decode for Event {
    fn decode(buf: Vec<u8>) -> Self {
        match buf[0] {
            0 => Self::Pointer(wl_pointer::Event::Motion {
                time: u32::from_ne_bytes(buf[1..5].try_into().unwrap()),
                surface_x: f64::from_ne_bytes(buf[5..13].try_into().unwrap()),
                surface_y: f64::from_ne_bytes(buf[13..21].try_into().unwrap()),
            }),
            1 => Self::Pointer(wl_pointer::Event::Button {
                serial: 0,
                time: (u32::from_ne_bytes(buf[1..5].try_into().unwrap())),
                button: (u32::from_ne_bytes(buf[5..9].try_into().unwrap())),
                state: (WEnum::Value(wl_pointer::ButtonState::try_from(buf[9] as u32).unwrap())),
            }),
            2 => Self::Pointer(wl_pointer::Event::Axis {
                time: (u32::from_ne_bytes(buf[1..5].try_into().unwrap())),
                axis: (WEnum::Value(wl_pointer::Axis::try_from(buf[5] as u32).unwrap())),
                value: (f64::from_ne_bytes(buf[6..14].try_into().unwrap())),
            }),
            3 => Self::Pointer(wl_pointer::Event::Frame {}),
            4 => Self::Keyboard(wl_keyboard::Event::Key {
                serial: 0,
                time: u32::from_ne_bytes(buf[1..5].try_into().unwrap()),
                key: u32::from_ne_bytes(buf[5..9].try_into().unwrap()),
                state: WEnum::Value(wl_keyboard::KeyState::try_from(buf[9] as u32).unwrap()),
            }),
            5 => Self::Keyboard(wl_keyboard::Event::Modifiers {
                serial: 0,
                mods_depressed: u32::from_ne_bytes(buf[1..5].try_into().unwrap()),
                mods_latched: u32::from_ne_bytes(buf[5..9].try_into().unwrap()),
                mods_locked: u32::from_ne_bytes(buf[9..13].try_into().unwrap()),
                group: u32::from_ne_bytes(buf[13..17].try_into().unwrap()),
            }),
            6 => Self::Release(),
            _ => panic!("protocol violation"),
        }
    }
}

