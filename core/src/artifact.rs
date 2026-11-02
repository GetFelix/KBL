//! Artifacts: payloads too big for the run log. They are stored once in the
//! artifacts cache under their content hash, and records carry the id.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Anything over this many bytes goes to an artifact instead of into a record.
pub const INLINE_LIMIT: usize = 4 * 1024;

/// An artifact over this many bytes is split into parts of this size.
pub const PART_SIZE: usize = 1024 * 1024;

const PREFIX: &str = "b-";
/// `b-` and the 43 characters of an unpadded base64url SHA-256.
const ID_LEN: usize = 45;

/// `b-` and the base64url SHA-256 of the content: 45 characters, inside the key rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ArtifactId(String);

/// A string that is not an artifact id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidArtifactId(pub String);

impl fmt::Display for InvalidArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} is not an artifact id", self.0)
    }
}

impl std::error::Error for InvalidArtifactId {}

impl ArtifactId {
    /// The id of `content`.
    pub fn of(content: &[u8]) -> Self {
        let digest = Sha256::digest(content);
        Self(format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(digest)))
    }

    pub fn parse(s: &str) -> Result<Self, InvalidArtifactId> {
        let ok = s.len() == ID_LEN
            // The engine rejects padding and stray trailing bits, so only the canonical form decodes.
            && s.strip_prefix(PREFIX).is_some_and(|hash| {
                URL_SAFE_NO_PAD.decode(hash).is_ok_and(|bytes| bytes.len() == 32)
            });
        if ok {
            Ok(Self(s.to_string()))
        } else {
            Err(InvalidArtifactId(s.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `content` hashes to this id. A split artifact's id is its manifest's, so this holds for the manifest, not the joined parts.
    pub fn matches(&self, content: &[u8]) -> bool {
        *self == Self::of(content)
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ArtifactId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::names::is_valid_key;

    #[test]
    fn id_of_known_content() {
        // SHA-256 of the empty string is e3b0c442...b855.
        let id = ArtifactId::of(b"");
        assert_eq!(id.as_str(), "b-47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU");
        assert_eq!(id.as_str().len(), 45);
        assert!(is_valid_key(id.as_str()));
        assert!(id.matches(b""));
        assert!(!id.matches(b"x"));
    }

    #[test]
    fn parse_accepts_only_real_ids() {
        let id = ArtifactId::of(b"hello");
        assert_eq!(ArtifactId::parse(id.as_str()).unwrap(), id);
        for bad in [
            "",
            "b-",
            "x-47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU",
            "b-short",
            &format!("{}=", id),
        ] {
            assert!(ArtifactId::parse(bad).is_err(), "{bad}");
        }
        // Same length, last character carries stray bits.
        let mut s = ArtifactId::of(b"").as_str().to_string();
        s.pop();
        s.push('W');
        assert!(ArtifactId::parse(&s).is_err());
    }
}
