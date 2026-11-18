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

/// Most parts one artifact is split into, so 16 MiB: the broker's frame limit.
pub const MAX_PARTS: usize = 16;

/// First line of a manifest. A stored blob that starts with it is always a manifest.
const MANIFEST_MAGIC: &str = "kbl-manifest/1\n";

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

/// How `bytes` are stored: one blob, or parts and the manifest that lists them.
pub struct Stored {
    /// The id records carry: the blob's, or the manifest's.
    pub id: ArtifactId,
    /// Every blob to put in the cache, the manifest last.
    pub blobs: Vec<(ArtifactId, Vec<u8>)>,
}

impl Stored {
    /// Splits `bytes` over [`PART_SIZE`]. Content that starts like a manifest
    /// is also wrapped in one, so a plain blob is never read as a manifest.
    /// Over [`MAX_PARTS`] parts is refused.
    pub fn of(bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.len() <= PART_SIZE && !bytes.starts_with(MANIFEST_MAGIC.as_bytes()) {
            let id = ArtifactId::of(&bytes);
            return Ok(Self {
                id: id.clone(),
                blobs: vec![(id, bytes)],
            });
        }
        if bytes.len() > PART_SIZE * MAX_PARTS {
            return Err(format!(
                "{} bytes is over the {} byte limit",
                bytes.len(),
                PART_SIZE * MAX_PARTS
            ));
        }
        let mut blobs: Vec<(ArtifactId, Vec<u8>)> = bytes
            .chunks(PART_SIZE)
            .map(|part| (ArtifactId::of(part), part.to_vec()))
            .collect();
        let mut manifest = String::from(MANIFEST_MAGIC);
        for (id, _) in &blobs {
            manifest.push_str(id.as_str());
            manifest.push('\n');
        }
        let manifest = manifest.into_bytes();
        let id = ArtifactId::of(&manifest);
        blobs.push((id.clone(), manifest));
        Ok(Self { id, blobs })
    }
}

/// The part ids a stored blob lists, or `None` when it is a plain artifact.
/// `Err` when it is a manifest that is malformed or lists too many parts.
pub fn parts_of(blob: &[u8]) -> Option<Result<Vec<ArtifactId>, InvalidArtifactId>> {
    let body = blob.strip_prefix(MANIFEST_MAGIC.as_bytes())?;
    let parsed = std::str::from_utf8(body)
        .map_err(|_| InvalidArtifactId("manifest".into()))
        .and_then(|text| {
            text.lines()
                .map(ArtifactId::parse)
                .collect::<Result<Vec<_>, _>>()
        })
        .and_then(|ids| {
            if ids.is_empty() || ids.len() > MAX_PARTS {
                Err(InvalidArtifactId("manifest".into()))
            } else {
                Ok(ids)
            }
        });
    Some(parsed)
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

    #[test]
    fn small_content_is_one_blob_and_big_content_splits_with_a_manifest() {
        let small = Stored::of(vec![7; PART_SIZE]).unwrap();
        assert_eq!(small.blobs.len(), 1);
        assert_eq!(small.id, small.blobs[0].0);

        let big = Stored::of(vec![7; 3 * 1024 * 1024]).unwrap();
        assert_eq!(big.blobs.len(), 4);
        let (manifest_id, manifest) = big.blobs.last().unwrap();
        assert_eq!(&big.id, manifest_id);
        assert!(manifest_id.matches(manifest));
        let ids = parts_of(manifest).unwrap().unwrap();
        assert_eq!(ids.len(), 3);
        assert!(
            big.blobs
                .iter()
                .take(3)
                .all(|(id, b)| ids.contains(id) && b.len() <= PART_SIZE)
        );

        assert!(Stored::of(vec![0; PART_SIZE * MAX_PARTS + 1]).is_err());
    }

    #[test]
    fn text_that_looks_like_a_manifest_is_wrapped_not_misread() {
        let text = format!("{MANIFEST_MAGIC}not ids").into_bytes();
        let stored = Stored::of(text.clone()).unwrap();
        assert_eq!(stored.blobs.len(), 2);
        assert_eq!(stored.blobs[0].1, text);
        assert!(parts_of(&text).unwrap().is_err());
    }
}
