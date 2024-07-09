use std::array::TryFromSliceError;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error(transparent)]
    MissingData(#[from] TryFromSliceError),
    #[error("invalid event id: `{0}`")]
    InvalidEventId(u8),
    #[error("invalid pointer event type: `{0}`")]
    InvalidPointerEventId(u8),
    #[error("invalid keyboard event type: `{0}`")]
    InvalidKeyboardEventId(u8),
    #[error("expected data at idx `{0:?}`")]
    Data(String),
}
