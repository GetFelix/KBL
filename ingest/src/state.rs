//! What ingest remembers between polls. It is kept in the tracker, so a
//! restart (or a second worker started by mistake) resumes without importing
//! again. The log stays the record of what happened; this is only bookkeeping.

use std::collections::{BTreeMap, BTreeSet};

use agents_core::names::ItemId;
use serde::{Deserialize, Serialize};

use crate::source::{Closed, Cursor};

/// One imported issue, by upstream id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Tracked {
    pub(crate) item: ItemId,
    /// The rule that admitted it.
    pub(crate) rule: String,
    /// Title, body and labels as last seen, hashed, to tell an edit from a
    /// change that touched none of them.
    pub(crate) fingerprint: String,
    pub(crate) title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) closed: Option<Closed>,
    /// Whether it still matches its rule.
    pub(crate) in_scope: bool,
    /// Comments already recorded, by id, with the update time they had.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) comments: BTreeMap<u64, String>,
    /// Issues it waits on that are not imported yet.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) pending_links: BTreeSet<String>,
    /// Issues it waits on that already have an edge.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub(crate) linked: BTreeSet<String>,
    /// Ingest itself closed the root, as opposed to a person. Only such a
    /// root is reopened when the issue is.
    #[serde(default)]
    pub(crate) closed_by_us: bool,
    /// The pull request already made a draft after a drop.
    #[serde(default)]
    pub(crate) written_back: bool,
    /// Set on the old id of a transferred issue, which is not fetched again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) moved_to: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct State {
    pub(crate) cursor: Cursor,
    /// Upstream id (`owner/repo#n`) to the issue. A transferred issue has an
    /// entry under each id.
    pub(crate) items: BTreeMap<String, Tracked>,
    /// Matches left out because their rule was at its cap, oldest first.
    pub(crate) waiting: BTreeSet<String>,
    /// Polls since the last reconcile.
    #[serde(default)]
    pub(crate) polls: u32,
}
