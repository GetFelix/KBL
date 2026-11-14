//! The work fold from `kbl-core` as WebAssembly, for the dashboard.
//!
//! The page feeds it the payloads it reads from `work.events` and renders what
//! it returns. It adds no rules of its own: the state is the same fold the
//! tracker runs, so the two cannot disagree about an item's stage.

use std::collections::BTreeMap;

use kbl_core::content::Content;
use kbl_core::overlap::{FILES_ENTRY, files_offset, overlaps};
use kbl_core::work::{ReadError, WorkLogged, WorkRecord, read};
use kbl_core::work_fold::State;
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

    /// Open items that expect to touch the same files, as
    /// `[{first, then, files}]`: `then` is suggested to wait on `first`.
    pub fn overlaps_json(&self) -> String {
        let found = overlaps(&self.state, |o| self.files.get(&o).map(String::as_str));
        serde_json::to_string(&found).expect("plain data serializes")
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
