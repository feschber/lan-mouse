use input_event::{Event as InputEvent, KeyboardEvent, PointerEvent};
use num_enum::{IntoPrimitive, TryFromPrimitive, TryFromPrimitiveError};
use paste::paste;
use std::{
    fmt::{Debug, Display, Formatter},
    mem::size_of,
};
use thiserror::Error;

/// Maximum payload bytes carried inside a single [`ClipboardChunk::Data`] packet.
/// Kept well below standard UDP/DTLS MTU (~1200 bytes) to prevent IP fragmentation.
pub const CLIPBOARD_CHUNK_DATA_LEN: usize = 512;

/// Maximum size an encoded protocol event can occupy on the wire.
/// Header (1 byte event type + 1 byte chunk kind + 4 bytes id + 4 bytes offset/len + 2 bytes data_len)
/// plus [`CLIPBOARD_CHUNK_DATA_LEN`].
pub const MAX_EVENT_SIZE: usize =
    size_of::<u8>() + size_of::<u8>() + size_of::<u32>() + size_of::<u32>() + size_of::<u16>()
        + CLIPBOARD_CHUNK_DATA_LEN;

/// Maximum total clipboard size accepted during chunk reassembly (4 MiB).
pub const MAX_CLIPBOARD_TOTAL_LEN: usize = 4 * 1024 * 1024;

/// error type for protocol violations
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// event type does not exist
    #[error("invalid event id: `{0}`")]
    InvalidEventId(#[from] TryFromPrimitiveError<EventType>),
    /// position type does not exist
    #[error("invalid event id: `{0}`")]
    InvalidPosition(#[from] TryFromPrimitiveError<Position>),
    /// clipboard chunk kind does not exist
    #[error("invalid clipboard chunk kind: `{0}`")]
    InvalidClipboardChunkKind(#[from] TryFromPrimitiveError<ClipboardChunkKind>),
    /// clipboard chunk length exceeds packet bounds
    #[error("invalid clipboard chunk data length: `{0}`")]
    InvalidClipboardChunkLength(u16),
}

/// Kind of Deskflow-style clipboard stream chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum ClipboardChunkKind {
    Start = 0,
    Data = 1,
    End = 2,
}

/// A single DTLS-safe chunk of a clipboard synchronization transfer
/// (modeled after Deskflow's `StreamChunker` / `ClipboardChunk`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipboardChunk {
    pub kind: ClipboardChunkKind,
    pub transfer_id: u32,
    /// - For [`ClipboardChunkKind::Start`]: total UTF-8 byte length of the clipboard payload.
    /// - For [`ClipboardChunkKind::Data`]: byte offset of this chunk within the payload.
    /// - For [`ClipboardChunkKind::End`]: 32-bit FNV-1a checksum of the full payload.
    pub offset_or_len: u32,
    pub data_len: u16,
    pub data: [u8; CLIPBOARD_CHUNK_DATA_LEN],
}

impl ClipboardChunk {
    pub fn start(transfer_id: u32, total_len: u32) -> Self {
        Self {
            kind: ClipboardChunkKind::Start,
            transfer_id,
            offset_or_len: total_len,
            data_len: 0,
            data: [0u8; CLIPBOARD_CHUNK_DATA_LEN],
        }
    }

    pub fn data(transfer_id: u32, offset: u32, slice: &[u8]) -> Self {
        assert!(slice.len() <= CLIPBOARD_CHUNK_DATA_LEN);
        let mut data = [0u8; CLIPBOARD_CHUNK_DATA_LEN];
        data[..slice.len()].copy_from_slice(slice);
        Self {
            kind: ClipboardChunkKind::Data,
            transfer_id,
            offset_or_len: offset,
            data_len: slice.len() as u16,
            data,
        }
    }

    pub fn end(transfer_id: u32, checksum: u32) -> Self {
        Self {
            kind: ClipboardChunkKind::End,
            transfer_id,
            offset_or_len: checksum,
            data_len: 0,
            data: [0u8; CLIPBOARD_CHUNK_DATA_LEN],
        }
    }

    pub fn payload(&self) -> &[u8] {
        let len = (self.data_len as usize).min(CLIPBOARD_CHUNK_DATA_LEN);
        &self.data[..len]
    }
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
#[derive(Clone, Copy, Debug)]
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
    /// Build identification for the sending peer. Sent by the
    /// connect side once after the connection authenticates, and
    /// echoed back by the listen side in reply, so each end can
    /// display the peer's build hash and warn (soft) on mismatch.
    /// `commit` is the 8-byte ASCII short commit hash from
    /// `shadow_rs`'s `SHORT_COMMIT`. Old peers that don't
    /// recognize the event type silently skip it per the
    /// forward-compat handling in the receive loop.
    Hello { commit: [u8; 8] },
    /// Deskflow-style chunked clipboard transfer event.
    Clipboard(ClipboardChunk),
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
            ProtoEvent::Hello { commit } => {
                let s = std::str::from_utf8(commit).unwrap_or("????????");
                write!(f, "Hello({s})")
            }
            ProtoEvent::Clipboard(chunk) => write!(
                f,
                "Clipboard({:?}, id={}, off/len={}, bytes={})",
                chunk.kind, chunk.transfer_id, chunk.offset_or_len, chunk.data_len
            ),
        }
    }
}

#[derive(TryFromPrimitive, IntoPrimitive)]
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
    Hello,
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
            },
            ProtoEvent::Ping => EventType::Ping,
            ProtoEvent::Pong(_) => EventType::Pong,
            ProtoEvent::Enter(_) => EventType::Enter,
            ProtoEvent::Leave(_) => EventType::Leave,
            ProtoEvent::Ack(_) => EventType::Ack,
            ProtoEvent::Hello { .. } => EventType::Hello,
            ProtoEvent::Clipboard(_) => EventType::Clipboard,
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
            EventType::Hello => {
                let mut commit = [0u8; 8];
                for b in commit.iter_mut() {
                    *b = decode_u8(&mut buf)?;
                }
                Ok(Self::Hello { commit })
            }
            EventType::Clipboard => {
                let kind: ClipboardChunkKind = decode_u8(&mut buf)?.try_into()?;
                let transfer_id = decode_u32(&mut buf)?;
                let offset_or_len = decode_u32(&mut buf)?;
                let data_len = decode_u16(&mut buf)?;
                if (data_len as usize) > CLIPBOARD_CHUNK_DATA_LEN {
                    return Err(ProtocolError::InvalidClipboardChunkLength(data_len));
                }
                let mut data = [0u8; CLIPBOARD_CHUNK_DATA_LEN];
                let len = data_len as usize;
                data[..len].copy_from_slice(&buf[..len]);
                Ok(Self::Clipboard(ClipboardChunk {
                    kind,
                    transfer_id,
                    offset_or_len,
                    data_len,
                    data,
                }))
            }
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
                },
                ProtoEvent::Ping => {}
                ProtoEvent::Pong(alive) => encode_u8(buf, len, alive as u8),
                ProtoEvent::Enter(pos) => encode_u8(buf, len, pos as u8),
                ProtoEvent::Leave(serial) => encode_u32(buf, len, serial),
                ProtoEvent::Ack(serial) => encode_u32(buf, len, serial),
                ProtoEvent::Hello { commit } => {
                    for b in commit.iter() {
                        encode_u8(buf, len, *b);
                    }
                }
                ProtoEvent::Clipboard(chunk) => {
                    encode_u8(buf, len, chunk.kind as u8);
                    encode_u32(buf, len, chunk.transfer_id);
                    encode_u32(buf, len, chunk.offset_or_len);
                    let dlen = (chunk.data_len as usize).min(CLIPBOARD_CHUNK_DATA_LEN);
                    encode_u16(buf, len, dlen as u16);
                    if dlen > 0 {
                        let data_slice = std::mem::take(buf);
                        let (dst, rest) = data_slice.split_at_mut(dlen);
                        dst.copy_from_slice(&chunk.data[..dlen]);
                        *len += dlen;
                        *buf = rest;
                    }
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
decode_impl!(u16);
decode_impl!(u32);
decode_impl!(i32);
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
encode_impl!(u16);
encode_impl!(u32);
encode_impl!(i32);
encode_impl!(f64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_chunk_roundtrip() {
        let payload = "שלום עולם - Deskflow StreamChunker in Lan Mouse!".as_bytes();
        let chunk = ClipboardChunk::data(42, 128, payload);
        let (buf, len): ([u8; MAX_EVENT_SIZE], usize) = ProtoEvent::Clipboard(chunk).into();
        assert_eq!(len, 12 + payload.len());

        let decoded = ProtoEvent::try_from(buf).expect("valid clipboard chunk");
        match decoded {
            ProtoEvent::Clipboard(c) => {
                assert_eq!(c.kind, ClipboardChunkKind::Data);
                assert_eq!(c.transfer_id, 42);
                assert_eq!(c.offset_or_len, 128);
                assert_eq!(c.payload(), payload);
            }
            other => panic!("expected Clipboard event, got {other:?}"),
        }
    }
}
