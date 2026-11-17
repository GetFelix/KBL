//! The text of the records a read shows, kept in memory up to a size cap.

use std::collections::{BTreeMap, HashMap};

use kbl_core::content::Content;
use kbl_core::work::WorkRecord;

/// Bytes of text kept before the least recently used go. A miss reads the
/// record back from the log, so the cap only trades memory for a read.
pub(super) const TEXT_CACHE_BYTES: usize = 8 * 1024 * 1024;

/// Least-recently-used texts by record offset, bounded by their size.
pub(super) struct Texts {
    entries: HashMap<u64, (Content, u64)>,
    /// Use order: the tick of each entry's last use, oldest first.
    order: BTreeMap<u64, u64>,
    tick: u64,
    bytes: usize,
    cap: usize,
}

impl Default for Texts {
    fn default() -> Self {
        Self::with_cap(TEXT_CACHE_BYTES)
    }
}

impl Texts {
    pub(super) fn with_cap(cap: usize) -> Self {
        Texts {
            entries: HashMap::new(),
            order: BTreeMap::new(),
            tick: 0,
            bytes: 0,
            cap,
        }
    }

    pub(super) fn insert(&mut self, offset: u64, text: Content) {
        self.remove(offset);
        self.tick += 1;
        self.bytes += size(&text);
        self.order.insert(self.tick, offset);
        self.entries.insert(offset, (text, self.tick));
        // The newest entry stays even when it alone passes the cap.
        while self.bytes > self.cap && self.entries.len() > 1 {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some((text, _)) = self.entries.remove(&oldest) {
                self.bytes -= size(&text);
            }
        }
    }

    /// The text at `offset`, marking it as just used.
    pub(super) fn get(&mut self, offset: u64) -> Option<&Content> {
        let (_, used) = self.entries.get_mut(&offset)?;
        self.order.remove(used);
        self.tick += 1;
        *used = self.tick;
        self.order.insert(self.tick, offset);
        self.entries.get(&offset).map(|(text, _)| text)
    }

    /// The text at `offset` without marking it used.
    #[cfg(test)]
    pub(super) fn peek(&self, offset: u64) -> Option<&Content> {
        self.entries.get(&offset).map(|(text, _)| text)
    }

    #[cfg(test)]
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub(super) fn set_cap(&mut self, cap: usize) {
        self.cap = cap;
    }

    fn remove(&mut self, offset: u64) {
        if let Some((text, used)) = self.entries.remove(&offset) {
            self.order.remove(&used);
            self.bytes -= size(&text);
        }
    }
}

fn size(text: &Content) -> usize {
    match text {
        Content::Text(t) => t.len(),
        Content::Artifact(_) => 64,
    }
}

/// The text a read shows for `record`, if it has one.
pub(super) fn text_of(record: &WorkRecord) -> Option<Content> {
    match record {
        WorkRecord::Context { text, .. }
        | WorkRecord::HumanInput { text, .. }
        | WorkRecord::Steered { text, .. }
        | WorkRecord::Question { text, .. } => Some(text.clone()),
        // An answer picks an option or says something, never both, so one entry reads back either.
        WorkRecord::Answer { text, chosen, .. } => Some(match chosen {
            Some(c) => Content::Text(c.clone()),
            None => text.clone(),
        }),
        // What the asker knew, which resume work reads as `so_far`.
        WorkRecord::Decision { so_far, .. } => Some(Content::Text(so_far.clone())),
        WorkRecord::Triaged { why, .. } => Some(Content::Text(why.clone())),
        WorkRecord::Prerequisite { new: Some(n), .. } => Some(n.why.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
