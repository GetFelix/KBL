//! Text that is either short enough to sit in a record or lives in an artifact.

use serde::{Deserialize, Serialize};

use crate::artifact::{ArtifactId, INLINE_LIMIT};

/// A value a record or a task carries. Up to [`INLINE_LIMIT`] bytes of text go
/// inline; anything bigger is an artifact the reader fetches on demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Content {
    Text(String),
    Artifact(ArtifactId),
}

impl Content {
    /// Inline when `text` fits, otherwise the artifact id and the text to store under it.
    pub fn from_text(text: String) -> (Self, Option<(ArtifactId, Vec<u8>)>) {
        if text.len() <= INLINE_LIMIT {
            (Content::Text(text), None)
        } else {
            let bytes = text.into_bytes();
            let id = ArtifactId::of(&bytes);
            (Content::Artifact(id.clone()), Some((id, bytes)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_text_stays_inline_and_big_text_moves() {
        let (c, store) = Content::from_text("hi".into());
        assert_eq!(c, Content::Text("hi".into()));
        assert!(store.is_none());

        let (c, store) = Content::from_text("x".repeat(INLINE_LIMIT));
        assert!(matches!(c, Content::Text(_)) && store.is_none());

        let (c, store) = Content::from_text("x".repeat(INLINE_LIMIT + 1));
        let (id, bytes) = store.unwrap();
        assert_eq!(c, Content::Artifact(id.clone()));
        assert!(id.matches(&bytes));
    }
}
