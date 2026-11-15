//! The work fold from `kbl-core` as WebAssembly, for the dashboard.
//!
//! The page feeds it the payloads it reads from `work.events` and renders what
//! it returns. It adds no rules of its own: the state is the same fold the
//! tracker runs, so the two cannot disagree about an item's stage.

use std::cell::RefCell;
use std::collections::BTreeMap;

use kbl_core::content::Content;
use kbl_core::names::ItemId;
use kbl_core::overlap::{FILES_ENTRY, files_offset, overlaps};
use kbl_core::work::{ReadError, WorkLogged, WorkRecord, read};
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
    /// The text of each item's current `files` entry, by its offset. The
    /// state keeps only the offset, and the page has no log to read it from.
    files: BTreeMap<u64, String>,
    /// `overlaps_json` as of the last applied record. The comparison is
    /// pairwise, so a redraw with no new record reuses it.
    overlaps: RefCell<Option<String>>,
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
                let before = self.files_offset(&rec);
                self.state.apply(offset, &rec);
                self.keep_files(offset, &rec, before);
                self.overlaps.take();
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

    /// The log's format so far: the highest a record may have. The page hides
    /// the writes a log below that format would refuse.
    pub fn format(&self) -> u32 {
        self.state.format()
    }

    /// The last offset applied, or -1 before the first.
    pub fn at(&self) -> f64 {
        self.state.at.map_or(-1.0, |at| at as f64)
    }

    /// The whole state, as the tracker would serialize it.
    pub fn state_json(&self) -> String {
        serde_json::to_string(&self.state).expect("plain data serializes")
    }

    /// Every item as `{id, title, stage, tier, proposed_tier, source, repo, needs,
    /// question, options, default, unclear, root}`, in id order. `tier`, `source` and
    /// `repo` are null unless recorded: `tier` by acceptance or a tier change,
    /// `proposed_tier` by triage, `source` and `repo` by the item's target, or its
    /// root's target or import. `needs` says why the item waits on a person;
    /// `question` is the offset of its open question, with that question's `options`
    /// and `default`, and `unclear` (`scope`, `repo` or `acceptance`) when it is
    /// triage's. `root` is true for an item with no parent. `accepted` and `flagged` say
    /// whether a person accepted it and whether one flagged it for a person.
    pub fn items_json(&self) -> String {
        let imported = self.state.imported_sources();
        let items: Vec<_> = self
            .state
            .items
            .values()
            .map(|i| {
                let root = self.state.items.get(&i.root);
                let target = i.target.as_ref().or(root.and_then(|r| r.target.as_ref()));
                let source = target
                    .map(|t| t.source.as_str())
                    .or_else(|| imported.get(&i.root).copied());
                json!({
                    "id": i.id,
                    "title": i.title,
                    "stage": self.state.stage(&i.id),
                    "tier": i.tier,
                    "proposed_tier": i.proposal.as_ref().map(|p| p.tier),
                    "source": source,
                    "repo": target.map(|t| &t.repo),
                    "needs": self.state.needs_you(&i.id),
                    "question": i.open_question,
                    "options": i.open_question.and(i.ask.as_ref()).map(|a| &a.options),
                    "default": i.open_question.and(i.ask.as_ref()).and_then(|a| a.default.as_ref()),
                    "unclear": i.open_question.and(i.ask.as_ref()).and_then(|a| a.unclear),
                    "root": i.parent.is_none(),
                    "accepted": i.accepted,
                    "flagged": i.flagged,
                })
            })
            .collect();
        serde_json::to_string(&items).expect("plain data serializes")
    }

    /// Every item in the Blocked stage, as `{id, title, repo, source, waits_on}`, in
    /// id order. `waits_on` lists the items holding it, each with its own
    /// `repo`, stage, `source` and `awaiting_release` (merged, but the link
    /// waits for the release), so a blocker in another repo or source shows
    /// where it lives. `repo` is `{source, repo}`, or null when the fold knows none.
    pub fn blocked_json(&self) -> String {
        let s = &self.state;
        let source_of = |id: &ItemId| {
            let root = &s.items.get(id)?.root;
            s.imported()
                .find(|(_, _, r)| *r == root)
                .map(|(src, _, _)| src)
        };
        let blocker = |id: &ItemId| {
            json!({
                "id": id,
                "title": s.items.get(id).map(|i| i.title.as_str()),
                "repo": s.repo_of(id),
                "stage": s.stage(id),
                "source": source_of(id),
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
                        "source": source_of(&i.id),
                        "waits_on": by.into_iter().map(blocker).collect::<Vec<_>>(),
                    })
                })
            })
            .collect();
        serde_json::to_string(&blocked).expect("plain data serializes")
    }

    /// Open items that expect to touch the same files, as
    /// `[{first, then, files}]`: `then` is suggested to wait on `first`.
    pub fn overlaps_json(&self) -> String {
        self.overlaps
            .borrow_mut()
            .get_or_insert_with(|| {
                let found = overlaps(&self.state, |o| self.files.get(&o).map(String::as_str));
                serde_json::to_string(&found).expect("plain data serializes")
            })
            .clone()
    }

    /// Claim costs summed per root, reviewer tier and repo: `Spend` from the core fold.
    pub fn spend_json(&self) -> String {
        serde_json::to_string(&self.state.spend()).expect("plain data serializes")
    }
}

impl Folder {
    /// The offset of the `files` entry `rec` would replace, if it writes one.
    fn files_offset(&self, rec: &WorkLogged) -> Option<u64> {
        match &rec.record {
            WorkRecord::Context { item, entry, .. } if entry.as_str() == FILES_ENTRY => {
                files_offset(self.state.items.get(item)?)
            }
            _ => None,
        }
    }

    /// Keeps the text of a `files` entry the fold took, and drops the one it replaced.
    fn keep_files(&mut self, offset: u64, rec: &WorkLogged, before: Option<u64>) {
        let WorkRecord::Context { item, entry, text } = &rec.record else {
            return;
        };
        let now = self.state.items.get(item).and_then(files_offset);
        if entry.as_str() != FILES_ENTRY || now != Some(offset) {
            return;
        }
        if let Some(old) = before {
            self.files.remove(&old);
        }
        if let Content::Text(t) = text {
            self.files.insert(offset, t.clone());
        }
    }
}
