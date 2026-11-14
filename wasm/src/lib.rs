//! The work fold from `kbl-core` as WebAssembly, for the dashboard.
//!
//! The page feeds it the payloads it reads from `work.events` and renders what
//! it returns. It adds no rules of its own: the state is the same fold the
//! tracker runs, so the two cannot disagree about an item's stage.

use kbl_core::names::ItemId;
use kbl_core::work::{ReadError, read};
use kbl_core::work_fold::{Stage, State};
use serde_json::json;
use wasm_bindgen::prelude::*;

/// A fold in progress. Offsets are numbers, exact up to 2^53 like the gateway's.
#[wasm_bindgen]
#[derive(Default)]
pub struct Folder {
    state: State,
    /// The format of the first record that was newer than this build reads.
    newer: Option<u32>,
}

#[wasm_bindgen]
impl Folder {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Folder {
        Folder::default()
    }

    /// Applies the record at `offset`. Returns false when the payload is not a
    /// record this build reads, or `offset` is not past the last one applied;
    /// the state is then unchanged. After a record of a newer format nothing
    /// more is applied: see [`Folder::newer`].
    pub fn apply(&mut self, offset: f64, payload: &[u8]) -> bool {
        let offset = offset as u64;
        if self.newer.is_some() || self.state.at.is_some_and(|at| offset <= at) {
            return false;
        }
        match read(payload) {
            Ok(rec) => {
                self.state.apply(offset, &rec);
                true
            }
            Err(ReadError::Newer { v }) => {
                self.newer = Some(v);
                false
            }
            Err(ReadError::Malformed(_)) => false,
        }
    }

    /// The format of the record that stopped the fold because this build is
    /// too old to read it, or 0. The page must stop and ask for a reload:
    /// the state no longer follows the log.
    pub fn newer(&self) -> u32 {
        self.newer.unwrap_or(0)
    }

    /// The last offset applied, or -1 before the first.
    pub fn at(&self) -> f64 {
        self.state.at.map_or(-1.0, |at| at as f64)
    }

    /// The whole state, as the tracker would serialize it.
    pub fn state_json(&self) -> String {
        serde_json::to_string(&self.state).expect("plain data serializes")
    }

    /// Every item as `{id, title, stage}`, in id order.
    pub fn items_json(&self) -> String {
        let items: Vec<_> = self
            .state
            .items
            .values()
            .map(|i| json!({ "id": i.id, "title": i.title, "stage": self.state.stage(&i.id) }))
            .collect();
        serde_json::to_string(&items).expect("plain data serializes")
    }

    /// Every item in the Blocked stage, as `{id, title, repo, waits_on}`, in
    /// id order. `waits_on` lists the items holding it, each with its own
    /// `repo`, stage, `source` and `awaiting_release` (merged, but the link
    /// waits for the release), so a blocker in another repo or source shows
    /// where it lives. `repo` is `{source, repo}`, or null when the fold knows none.
    pub fn blocked_json(&self) -> String {
        let s = &self.state;
        let blocker = |id: &ItemId| {
            json!({
                "id": id,
                "title": s.items.get(id).map(|i| i.title.as_str()),
                "repo": s.repo_of(id),
                "stage": s.stage(id),
                "source": s.items.get(id).and_then(|i| {
                    s.imported().find(|(_, _, r)| **r == i.root).map(|(src, _, _)| src)
                }),
                "awaiting_release": s.items.get(id).is_some_and(|i| i.closed.is_some()),
            })
        };
        let blocked: Vec<_> = s
            .items
            .values()
            .filter(|i| i.closed.is_none() && s.stage(&i.id) == Some(Stage::Blocked))
            .filter_map(|i| {
                let by = s.blockers(&i.id);
                (!by.is_empty()).then(|| {
                    json!({
                        "id": i.id,
                        "title": i.title,
                        "repo": s.repo_of(&i.id),
                        "waits_on": by.into_iter().map(blocker).collect::<Vec<_>>(),
                    })
                })
            })
            .collect();
        serde_json::to_string(&blocked).expect("plain data serializes")
    }

    /// Claim costs summed per root, reviewer tier and repo: `Spend` from the core fold.
    pub fn spend_json(&self) -> String {
        serde_json::to_string(&self.state.spend()).expect("plain data serializes")
    }
}
