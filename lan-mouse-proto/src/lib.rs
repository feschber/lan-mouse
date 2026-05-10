use input_event::{ClipboardEvent, Event as InputEvent, KeyboardEvent, PointerEvent};
use num_enum::{IntoPrimitive, TryFromPrimitive, TryFromPrimitiveError};
use paste::paste;
use std::{
    fmt::{Debug, Display, Formatter},
    mem::size_of,
};
use thiserror::Error;

/// defines the maximum size a fixed-buffer encoded event can take up.
/// All non-clipboard events fit in this size; clipboard events use the
/// variable-length [`encode_clipboard_event`] / [`decode_clipboard_event`]
/// helpers because their payload (originator fingerprint + content)
/// vastly exceeds the fixed buffer's capacity.
pub const MAX_EVENT_SIZE: usize = size_of::<u8>() + size_of::<u32>() + 2 * size_of::<f64>();

/// Maximum total clipboard payload size on the wire (originator
/// fingerprint + content + length prefixes). 4 KiB is conservative
/// against typical UDP MTU.
pub const MAX_CLIPBOARD_SIZE: usize = 4 * 1024;

/// error type for protocol violations
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// event type does not exist
    #[error("invalid event id: `{0}`")]
    InvalidEventId(#[from] TryFromPrimitiveError<EventType>),
    /// position type does not exist
    #[error("invalid event id: `{0}`")]
    InvalidPosition(#[from] TryFromPrimitiveError<Position>),
    /// clipboard payload exceeds [`MAX_CLIPBOARD_SIZE`]
    #[error("clipboard payload too large: {0} bytes")]
    ClipboardTooLarge(usize),
    /// clipboard text is not valid UTF-8
    #[error("invalid UTF-8 in clipboard payload")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
    /// not enough bytes left in the buffer
    #[error("buffer too small for clipboard payload")]
    BufferTooSmall,
}

/// Position of a client
#[derive(Clone, Copy, Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum Position {
    Left,
    Right,
    Top,
    Bottom,
}

impl Display for Position {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let pos = match self {
            Position::Left => "left",
            Position::Right => "right",
            Position::Top => "top",
            Position::Bottom => "bottom",
        };
        write!(f, "{pos}")
    }
}

/// main lan-mouse protocol event type
#[derive(Clone, Debug)]
pub enum ProtoEvent {
    /// notify a client that the cursor entered its region at the given position
    /// [`ProtoEvent::Ack`] with the same serial is used for synchronization between devices
    Enter(Position),
    /// notify a client that the cursor left its region
    /// [`ProtoEvent::Ack`] with the same serial is used for synchronization between devices
    Leave(u32),
    /// acknowledge of an [`ProtoEvent::Enter`] or [`ProtoEvent::Leave`] event
    Ack(u32),
    /// Input event
    Input(InputEvent),
    /// Ping event for tracking unresponsive clients.
    /// A client has to respond with [`ProtoEvent::Pong`].
    Ping,
    /// Response to [`ProtoEvent::Ping`], true if emulation is enabled / available
    Pong(bool),
    /// Display geometry of the receiving device. Sent by the
    /// emulation side immediately after the [`ProtoEvent::Ack`] of
    /// an [`ProtoEvent::Enter`] so the capturing peer can model the
    /// guest cursor's position along the entry axis. Width and
    /// height are in pixels of the union of all displays on the
    /// emulating device.
    Bounds { width: u32, height: u32 },
    /// Absolute cursor warp on the receiving device. Sent by the
    /// capturing peer after [`ProtoEvent::Enter`] so the guest's
    /// cursor lands at the position that visually corresponds to
    /// where the user's physical cursor was at the moment of
    /// crossing. `x` and `y` are pixel coordinates in the receiver's
    /// screen space, computed by the capturing peer using its own
    /// display bounds and the receiver-supplied [`ProtoEvent::Bounds`]
    /// from a prior Enter.
    MotionAbsolute { x: i32, y: i32 },
    /// Self-sufficient counterpart to [`ProtoEvent::MotionAbsolute`].
    /// Carries the host's cursor position normalized to the host's
    /// own display bounds (0..1 along each axis) plus the entry
    /// side from the receiver's frame. The receiver scales nx/ny
    /// against its own bounds and pins the on-axis dimension to
    /// the entry edge, eliminating the bootstrap problem where
    /// MotionAbsolute couldn't be sent on the first crossing
    /// because the host had no cached peer geometry.
    CursorPos { pos: Position, nx: f32, ny: f32 },
    /// Build identification for the sending peer. Sent by the
    /// connect side once after the connection authenticates, and
    /// echoed back by the listen side in reply, so each end can
    /// display the peer's build hash and warn (soft) on mismatch.
    /// `commit` is the 8-byte ASCII short commit hash from
    /// `shadow_rs`'s `SHORT_COMMIT`. Old peers that don't
    /// recognize the event type silently skip it per the
    /// forward-compat handling in the receive loop.
    Hello { commit: [u8; 8] },
    /// The receiver's per-pair motion-sensitivity multiplier.
    /// Sent by the emulating peer immediately after the
    /// [`ProtoEvent::Ack`] of an [`ProtoEvent::Enter`] so the
    /// capturing peer can scale its wall-press auto-release model
    /// to match. Without this, a sensitivity multiplier below 1.0
    /// would make the host's model accumulate "wall pressure"
    /// faster than the receiver's actual cursor moves, firing
    /// AutoRelease before the cursor has reached the edge. Old
    /// peers that don't recognize the event type silently skip it
    /// per the existing forward-compat handling.
    ReceiverSensitivity { mouse_sensitivity: f64 },
    /// Clipboard text content propagated from the originating peer.
    /// `from_fingerprint` is the TLS fingerprint of the peer that
    /// originally read the clipboard (not necessarily the sender —
    /// intermediate peers preserve the originator field when they
    /// fan-out to other peers). The receiver uses it to short-circuit
    /// the N-peer forwarding loop along with a recent-content cache.
    /// `content` is the clipboard text. Encoded with the variable-
    /// length [`encode_clipboard_event`] / [`decode_clipboard_event`]
    /// helpers; the fixed-buffer codec panics on this variant.
    Clipboard {
        from_fingerprint: String,
        content: String,
    },
}

impl Display for ProtoEvent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoEvent::Enter(s) => write!(f, "Enter({s})"),
            ProtoEvent::Leave(s) => write!(f, "Leave({s})"),
            ProtoEvent::Ack(s) => write!(f, "Ack({s})"),
            ProtoEvent::Input(e) => write!(f, "{e}"),
            ProtoEvent::Ping => write!(f, "ping"),
            ProtoEvent::Pong(alive) => {
                write!(
                    f,
                    "pong: {}",
                    if *alive { "alive" } else { "not available" }
                )
            }
            ProtoEvent::Bounds { width, height } => write!(f, "Bounds({width}x{height})"),
            ProtoEvent::MotionAbsolute { x, y } => write!(f, "MotionAbsolute({x}, {y})"),
            ProtoEvent::CursorPos { pos, nx, ny } => {
                write!(f, "CursorPos({pos}, {nx:.4}, {ny:.4})")
            }
            ProtoEvent::ReceiverSensitivity { mouse_sensitivity } => {
                write!(f, "ReceiverSensitivity({mouse_sensitivity:.2})")
            }
            ProtoEvent::Hello { commit } => {
                let s = std::str::from_utf8(commit).unwrap_or("????????");
                write!(f, "Hello({s})")
            }
            ProtoEvent::Clipboard {
                from_fingerprint,
                content,
            } => {
                let head: String = content.chars().take(40).collect();
                let preview = if head.len() < content.len() {
                    format!("{head}…")
                } else {
                    head
                };
                write!(
                    f,
                    "Clipboard(from={}…, {}b: {preview})",
                    &from_fingerprint[..from_fingerprint.len().min(8)],
                    content.len(),
                )
            }
        }
    }
}

#[derive(TryFromPrimitive, IntoPrimitive, Debug)]
#[repr(u8)]
pub enum EventType {
    PointerMotion,
    PointerButton,
    PointerAxis,
    PointerAxisValue120,
    KeyboardKey,
    KeyboardModifiers,
    Ping,
    Pong,
    Enter,
    Leave,
    Ack,
    Bounds,
    MotionAbsolute,
    CursorPos,
    Hello,
    ReceiverSensitivity,
    /// Variable-length clipboard frame; not decodable through the
    /// fixed-size [`MAX_EVENT_SIZE`] buffer path. See
    /// [`decode_clipboard_event`].
    Clipboard,
}

impl ProtoEvent {
    fn event_type(&self) -> EventType {
        match self {
            ProtoEvent::Input(e) => match e {
                InputEvent::Pointer(p) => match p {
                    PointerEvent::Motion { .. } => EventType::PointerMotion,
                    PointerEvent::Button { .. } => EventType::PointerButton,
                    PointerEvent::Axis { .. } => EventType::PointerAxis,
                    PointerEvent::AxisDiscrete120 { .. } => EventType::PointerAxisValue120,
                },
                InputEvent::Keyboard(k) => match k {
                    KeyboardEvent::Key { .. } => EventType::KeyboardKey,
                    KeyboardEvent::Modifiers { .. } => EventType::KeyboardModifiers,
                },
                InputEvent::Clipboard(c) => match c {
                    ClipboardEvent::Text(_) => EventType::Clipboard,
                },
            },
            ProtoEvent::Ping => EventType::Ping,
            ProtoEvent::Pong(_) => EventType::Pong,
            ProtoEvent::Enter(_) => EventType::Enter,
            ProtoEvent::Leave(_) => EventType::Leave,
            ProtoEvent::Ack(_) => EventType::Ack,
            ProtoEvent::Bounds { .. } => EventType::Bounds,
            ProtoEvent::MotionAbsolute { .. } => EventType::MotionAbsolute,
            ProtoEvent::CursorPos { .. } => EventType::CursorPos,
            ProtoEvent::Hello { .. } => EventType::Hello,
            ProtoEvent::ReceiverSensitivity { .. } => EventType::ReceiverSensitivity,
            ProtoEvent::Clipboard { .. } => EventType::Clipboard,
        }
    }
}

impl TryFrom<[u8; MAX_EVENT_SIZE]> for ProtoEvent {
    type Error = ProtocolError;

    fn try_from(buf: [u8; MAX_EVENT_SIZE]) -> Result<Self, Self::Error> {
        let mut buf = &buf[..];
        let event_type = decode_u8(&mut buf)?;
        match EventType::try_from(event_type)? {
            EventType::PointerMotion => {
                Ok(Self::Input(InputEvent::Pointer(PointerEvent::Motion {
                    time: decode_u32(&mut buf)?,
                    dx: decode_f64(&mut buf)?,
                    dy: decode_f64(&mut buf)?,
                })))
            }
            EventType::PointerButton => {
                Ok(Self::Input(InputEvent::Pointer(PointerEvent::Button {
                    time: decode_u32(&mut buf)?,
                    button: decode_u32(&mut buf)?,
                    state: decode_u32(&mut buf)?,
                })))
            }
            EventType::PointerAxis => Ok(Self::Input(InputEvent::Pointer(PointerEvent::Axis {
                time: decode_u32(&mut buf)?,
                axis: decode_u8(&mut buf)?,
                value: decode_f64(&mut buf)?,
            }))),
            EventType::PointerAxisValue120 => Ok(Self::Input(InputEvent::Pointer(
                PointerEvent::AxisDiscrete120 {
                    axis: decode_u8(&mut buf)?,
                    value: decode_i32(&mut buf)?,
                },
            ))),
            EventType::KeyboardKey => Ok(Self::Input(InputEvent::Keyboard(KeyboardEvent::Key {
                time: decode_u32(&mut buf)?,
                key: decode_u32(&mut buf)?,
                state: decode_u8(&mut buf)?,
            }))),
            EventType::KeyboardModifiers => Ok(Self::Input(InputEvent::Keyboard(
                KeyboardEvent::Modifiers {
                    depressed: decode_u32(&mut buf)?,
                    latched: decode_u32(&mut buf)?,
                    locked: decode_u32(&mut buf)?,
                    group: decode_u32(&mut buf)?,
                },
            ))),
            EventType::Ping => Ok(Self::Ping),
            EventType::Pong => Ok(Self::Pong(decode_u8(&mut buf)? != 0)),
            EventType::Enter => Ok(Self::Enter(decode_u8(&mut buf)?.try_into()?)),
            EventType::Leave => Ok(Self::Leave(decode_u32(&mut buf)?)),
            EventType::Ack => Ok(Self::Ack(decode_u32(&mut buf)?)),
            EventType::Bounds => Ok(Self::Bounds {
                width: decode_u32(&mut buf)?,
                height: decode_u32(&mut buf)?,
            }),
            EventType::MotionAbsolute => Ok(Self::MotionAbsolute {
                x: decode_i32(&mut buf)?,
                y: decode_i32(&mut buf)?,
            }),
            EventType::CursorPos => Ok(Self::CursorPos {
                pos: decode_u8(&mut buf)?.try_into()?,
                nx: decode_f32(&mut buf)?,
                ny: decode_f32(&mut buf)?,
            }),
            EventType::Hello => {
                let mut commit = [0u8; 8];
                for b in commit.iter_mut() {
                    *b = decode_u8(&mut buf)?;
                }
                Ok(Self::Hello { commit })
            }
            EventType::ReceiverSensitivity => Ok(Self::ReceiverSensitivity {
                mouse_sensitivity: decode_f64(&mut buf)?,
            }),
            // Clipboard frames are variable-length and never arrive
            // through the fixed-size buffer path; the connect/listen
            // layer routes them through `decode_clipboard_event`.
            EventType::Clipboard => Err(ProtocolError::BufferTooSmall),
        }
    }
}

impl From<ProtoEvent> for ([u8; MAX_EVENT_SIZE], usize) {
    fn from(event: ProtoEvent) -> Self {
        let mut buf = [0u8; MAX_EVENT_SIZE];
        let mut len = 0usize;
        {
            let mut buf = &mut buf[..];
            let buf = &mut buf;
            let len = &mut len;
            encode_u8(buf, len, event.event_type() as u8);
            match event {
                ProtoEvent::Input(event) => match event {
                    InputEvent::Pointer(p) => match p {
                        PointerEvent::Motion { time, dx, dy } => {
                            encode_u32(buf, len, time);
                            encode_f64(buf, len, dx);
                            encode_f64(buf, len, dy);
                        }
                        PointerEvent::Button {
                            time,
                            button,
                            state,
                        } => {
                            encode_u32(buf, len, time);
                            encode_u32(buf, len, button);
                            encode_u32(buf, len, state);
                        }
                        PointerEvent::Axis { time, axis, value } => {
                            encode_u32(buf, len, time);
                            encode_u8(buf, len, axis);
                            encode_f64(buf, len, value);
                        }
                        PointerEvent::AxisDiscrete120 { axis, value } => {
                            encode_u8(buf, len, axis);
                            encode_i32(buf, len, value);
                        }
                    },
                    InputEvent::Keyboard(k) => match k {
                        KeyboardEvent::Key { time, key, state } => {
                            encode_u32(buf, len, time);
                            encode_u32(buf, len, key);
                            encode_u8(buf, len, state);
                        }
                        KeyboardEvent::Modifiers {
                            depressed,
                            latched,
                            locked,
                            group,
                        } => {
                            encode_u32(buf, len, depressed);
                            encode_u32(buf, len, latched);
                            encode_u32(buf, len, locked);
                            encode_u32(buf, len, group);
                        }
                    },
                    InputEvent::Clipboard(_) => {
                        panic!(
                            "ProtoEvent::Input(Clipboard) cannot use the fixed-buffer \
                             encoder; route via encode_clipboard_event"
                        );
                    }
                },
                ProtoEvent::Ping => {}
                ProtoEvent::Pong(alive) => encode_u8(buf, len, alive as u8),
                ProtoEvent::Enter(pos) => encode_u8(buf, len, pos as u8),
                ProtoEvent::Leave(serial) => encode_u32(buf, len, serial),
                ProtoEvent::Ack(serial) => encode_u32(buf, len, serial),
                ProtoEvent::Bounds { width, height } => {
                    encode_u32(buf, len, width);
                    encode_u32(buf, len, height);
                }
                ProtoEvent::MotionAbsolute { x, y } => {
                    encode_i32(buf, len, x);
                    encode_i32(buf, len, y);
                }
                ProtoEvent::CursorPos { pos, nx, ny } => {
                    encode_u8(buf, len, pos as u8);
                    encode_f32(buf, len, nx);
                    encode_f32(buf, len, ny);
                }
                ProtoEvent::Hello { commit } => {
                    for b in commit.iter() {
                        encode_u8(buf, len, *b);
                    }
                }
                ProtoEvent::ReceiverSensitivity { mouse_sensitivity } => {
                    encode_f64(buf, len, mouse_sensitivity);
                }
                ProtoEvent::Clipboard { .. } => {
                    panic!(
                        "ProtoEvent::Clipboard cannot use the fixed-buffer encoder; \
                         route via encode_clipboard_event"
                    );
                }
            }
        }
        (buf, len)
    }
}

macro_rules! decode_impl {
    ($t:ty) => {
        paste! {
            fn [<decode_ $t>](data: &mut &[u8]) -> Result<$t, ProtocolError> {
                let (int_bytes, rest) = data.split_at(size_of::<$t>());
                *data = rest;
                Ok($t::from_be_bytes(int_bytes.try_into().unwrap()))
            }
        }
    };
}

decode_impl!(u8);
decode_impl!(u32);
decode_impl!(i32);
decode_impl!(f32);
decode_impl!(f64);

macro_rules! encode_impl {
    ($t:ty) => {
        paste! {
            fn [<encode_ $t>](buf: &mut &mut [u8], amt: &mut usize, n: $t) {
                let src = n.to_be_bytes();
                let data = std::mem::take(buf);
                let (int_bytes, rest) = data.split_at_mut(size_of::<$t>());
                int_bytes.copy_from_slice(&src);
                *amt += size_of::<$t>();
                *buf = rest
            }
        }
    };
}

encode_impl!(u8);
encode_impl!(u32);
encode_impl!(i32);
encode_impl!(f32);
encode_impl!(f64);

/// Wire format for clipboard frames:
/// `[event_type: u8][fp_len: u32 BE][fp: utf8][text_len: u32 BE][text: utf8]`
///
/// Returns the encoded bytes ready for transmission. The total
/// length is bounded by [`MAX_CLIPBOARD_SIZE`].
pub fn encode_clipboard_event(event: &ProtoEvent) -> Result<Vec<u8>, ProtocolError> {
    let (from_fingerprint, content) = match event {
        ProtoEvent::Clipboard {
            from_fingerprint,
            content,
        } => (from_fingerprint.as_str(), content.as_str()),
        ProtoEvent::Input(InputEvent::Clipboard(ClipboardEvent::Text(content))) => {
            // Convenience: capture-side callers carry only the text;
            // the originator fingerprint is empty until the service
            // layer stamps it in. Phase 2 wires the stamp.
            ("", content.as_str())
        }
        _ => panic!("encode_clipboard_event called on non-clipboard event"),
    };
    let fp_bytes = from_fingerprint.as_bytes();
    let text_bytes = content.as_bytes();
    let total = 1 + 4 + fp_bytes.len() + 4 + text_bytes.len();
    if total > MAX_CLIPBOARD_SIZE {
        return Err(ProtocolError::ClipboardTooLarge(total));
    }
    let mut buf = Vec::with_capacity(total);
    buf.push(EventType::Clipboard as u8);
    buf.extend_from_slice(&(fp_bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(fp_bytes);
    buf.extend_from_slice(&(text_bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(text_bytes);
    Ok(buf)
}

/// Decode a clipboard frame produced by [`encode_clipboard_event`].
pub fn decode_clipboard_event(buf: &[u8]) -> Result<ProtoEvent, ProtocolError> {
    if buf.len() > MAX_CLIPBOARD_SIZE {
        return Err(ProtocolError::ClipboardTooLarge(buf.len()));
    }
    if buf.is_empty() {
        return Err(ProtocolError::BufferTooSmall);
    }
    let tag = buf[0];
    let event_type = EventType::try_from(tag)?;
    if !matches!(event_type, EventType::Clipboard) {
        // Wrong-type tag in the clipboard channel — treat as a buffer
        // mismatch rather than silently producing some other variant.
        return Err(ProtocolError::BufferTooSmall);
    }
    let mut cursor = 1usize;
    if buf.len() < cursor + 4 {
        return Err(ProtocolError::BufferTooSmall);
    }
    let fp_len = u32::from_be_bytes([
        buf[cursor],
        buf[cursor + 1],
        buf[cursor + 2],
        buf[cursor + 3],
    ]) as usize;
    cursor += 4;
    if buf.len() < cursor + fp_len + 4 {
        return Err(ProtocolError::BufferTooSmall);
    }
    let from_fingerprint = String::from_utf8(buf[cursor..cursor + fp_len].to_vec())?;
    cursor += fp_len;
    let text_len = u32::from_be_bytes([
        buf[cursor],
        buf[cursor + 1],
        buf[cursor + 2],
        buf[cursor + 3],
    ]) as usize;
    cursor += 4;
    if buf.len() < cursor + text_len {
        return Err(ProtocolError::BufferTooSmall);
    }
    let content = String::from_utf8(buf[cursor..cursor + text_len].to_vec())?;
    Ok(ProtoEvent::Clipboard {
        from_fingerprint,
        content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_round_trip() {
        let event = ProtoEvent::Clipboard {
            from_fingerprint: "abcd1234".into(),
            content: "hello, world".into(),
        };
        let bytes = encode_clipboard_event(&event).expect("encode");
        let decoded = decode_clipboard_event(&bytes).expect("decode");
        match decoded {
            ProtoEvent::Clipboard {
                from_fingerprint,
                content,
            } => {
                assert_eq!(from_fingerprint, "abcd1234");
                assert_eq!(content, "hello, world");
            }
            other => panic!("expected Clipboard, got {other}"),
        }
    }

    #[test]
    fn clipboard_too_large_rejected() {
        let event = ProtoEvent::Clipboard {
            from_fingerprint: "fp".into(),
            content: "x".repeat(MAX_CLIPBOARD_SIZE),
        };
        assert!(matches!(
            encode_clipboard_event(&event),
            Err(ProtocolError::ClipboardTooLarge(_))
        ));
    }

    #[test]
    fn clipboard_decode_truncated() {
        // Encode then truncate the trailing content bytes; decoder
        // must surface BufferTooSmall instead of returning a bogus
        // string with random capture from the underlying memory.
        let event = ProtoEvent::Clipboard {
            from_fingerprint: "fp".into(),
            content: "some text".into(),
        };
        let bytes = encode_clipboard_event(&event).expect("encode");
        let truncated = &bytes[..bytes.len() - 1];
        assert!(matches!(
            decode_clipboard_event(truncated),
            Err(ProtocolError::BufferTooSmall)
        ));
    }
}
