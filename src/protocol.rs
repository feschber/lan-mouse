use wayland_client::protocol::{
    wl_pointer::{Axis, ButtonState},
    wl_keyboard::KeyState,
};

pub enum Event {
    Mouse{t: u32, x: f64, y: f64},
    Button{t: u32, b: u32, s: ButtonState},
    Axis{t: u32, a: Axis, v: f64},
    Key{t: u32, k: u32, s: KeyState},
    KeyModifier{mods_depressed: u32, mods_latched: u32, mods_locked: u32, group: u32},
}

impl Event {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Event::Mouse { t, x, y } => {
                let mut buf = Vec::new();
                buf.push(0u8);
                buf.extend_from_slice(t.to_ne_bytes().as_ref());
                buf.extend_from_slice(x.to_ne_bytes().as_ref());
                buf.extend_from_slice(y.to_ne_bytes().as_ref());
                buf
            }
            Event::Button { t, b, s } => {
                let mut buf = Vec::new();
                buf.push(1u8);
                buf.extend_from_slice(t.to_ne_bytes().as_ref());
                buf.extend_from_slice(b.to_ne_bytes().as_ref());
                buf.push(match s {
                    ButtonState::Released => 0u8, 
                    ButtonState::Pressed => 1u8, 
                    _ => todo!()
                });
                buf
            }
            Event::Axis{t, a, v} => {
                let mut buf = Vec::new();
                buf.push(2u8);
                buf.extend_from_slice(t.to_ne_bytes().as_ref());
                buf.push(match a {
                    Axis::VerticalScroll => 0,
                    Axis::HorizontalScroll => 1,
                    _ => todo!()
                });
                buf.extend_from_slice(v.to_ne_bytes().as_ref());
                buf
            }
            Event::Key{t, k, s } => {
                let mut buf = Vec::new();
                buf.push(3u8);
                buf.extend_from_slice(t.to_ne_bytes().as_ref());
                buf.extend_from_slice(k.to_ne_bytes().as_ref());
                buf.push(match s {
                    KeyState::Released => 0, 
                    KeyState::Pressed => 1, 
                    _ => todo!(),
                });
                buf
            }
            Event::KeyModifier{ mods_depressed, mods_latched, mods_locked, group } => {
                let mut buf = Vec::new();
                buf.push(4u8);
                buf.extend_from_slice(mods_depressed.to_ne_bytes().as_ref());
                buf.extend_from_slice(mods_latched.to_ne_bytes().as_ref());
                buf.extend_from_slice(mods_locked.to_ne_bytes().as_ref());
                buf.extend_from_slice(group.to_ne_bytes().as_ref());
                buf
            }
        }
    }

    pub fn decode(buf: [u8; 21]) -> Event {
        match buf[0] {
            0 => Self::Mouse {
                t: u32::from_ne_bytes(buf[1..5].try_into().unwrap()),
                x: f64::from_ne_bytes(buf[5..13].try_into().unwrap()),
                y: f64::from_ne_bytes(buf[13..21].try_into().unwrap()),
            },
            1 => Self::Button {
                t: (u32::from_ne_bytes(buf[1..5].try_into().unwrap())),
                b: (u32::from_ne_bytes(buf[5..9].try_into().unwrap())),
                s: (match buf[9] {
                    0 => ButtonState::Released,
                    1 => ButtonState::Pressed,
                    _ => panic!("protocol violation")
                })
            },
            2 => Self::Axis {
                t: (u32::from_ne_bytes(buf[1..5].try_into().unwrap())),
                a: (match buf[5] {
                    0 => Axis::VerticalScroll,
                    1 => Axis::HorizontalScroll,
                    _ => todo!()
                }),
                v: (f64::from_ne_bytes(buf[6..14].try_into().unwrap())),
            },
            3 => Self::Key {
                t: u32::from_ne_bytes(buf[1..5].try_into().unwrap()),
                k: u32::from_ne_bytes(buf[5..9].try_into().unwrap()),
                s: match buf[9] {
                    0 => KeyState::Released,
                    1 => KeyState::Pressed,
                    _ => todo!(),
                }
            },
            4 => Self::KeyModifier {
                mods_depressed: u32::from_ne_bytes(buf[1..5].try_into().unwrap()),
                mods_latched: u32::from_ne_bytes(buf[5..9].try_into().unwrap()),
                mods_locked: u32::from_ne_bytes(buf[9..13].try_into().unwrap()),
                group: u32::from_ne_bytes(buf[13..17].try_into().unwrap()),
            },
            _ => panic!("protocol violation"),
        }
    }
}
