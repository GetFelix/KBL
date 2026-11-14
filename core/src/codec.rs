//! Records and stored values are JSON. A reader ignores fields it does not
//! know, so a newer writer can add one without breaking an older reader.

use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Bytes that are not a valid value of the type asked for.
#[derive(Debug)]
pub struct DecodeError(serde_json::Error);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot decode: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

/// `encode` and `decode` for every format in this crate.
pub trait Codec: Serialize + DeserializeOwned + Sized {
    fn encode(&self) -> Vec<u8> {
        // Every type here is plain data with string keys, which JSON always holds.
        serde_json::to_vec(self).expect("plain data serializes")
    }

    fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        serde_json::from_slice(bytes).map_err(DecodeError)
    }
}

impl<T: Serialize + DeserializeOwned> Codec for T {}

/// The `v` of a payload that failed to decode, when it is above `reads`: the
/// payload is of a newer format, not malformed.
pub(crate) fn newer_format(payload: &[u8], reads: u32) -> Option<u32> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        v: u64,
    }
    let Envelope { v } = serde_json::from_slice(payload).ok()?;
    (v > u64::from(reads)).then(|| u32::try_from(v).unwrap_or(u32::MAX))
}
