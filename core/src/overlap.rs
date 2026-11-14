//! Open items that expect to touch the same files.
//!
//! An item declares the files it expects to change in its `files` context
//! entry, one path per line. Agents write it with `work_context`, people with
//! a context record. Two open items whose entries share a path are an overlap,
//! and the one further along is suggested to go first.
//!
//! This is a view over a folded state: it adds no record and leaves the fold
//! unchanged. The fold keeps only the offset of each context entry, so the
//! caller passes the text it read at that offset.

use serde::Serialize;

use crate::names::ItemId;
use crate::work_fold::{Item, Stage, State};

/// The context entry that names the files an item expects to touch.
pub const FILES_ENTRY: &str = "files";

/// Two open items that expect to touch the same files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Overlap {
    /// The item suggested to go first.
    pub first: ItemId,
    /// The item suggested to wait on `first`.
    pub then: ItemId,
    /// The paths both name, the more specific one where a directory covers a file.
    pub files: Vec<String>,
}

/// The paths in a `files` entry: one per line, `./` and surrounding space
/// trimmed, blank lines and `#` comments skipped. A path ending in `/` is a
/// directory and covers everything under it.
pub fn parse_files(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .lines()
        .map(|l| l.trim().trim_start_matches("./"))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The offset of the item's current `files` entry, if it has one.
pub fn files_offset(item: &Item) -> Option<u64> {
    item.context
        .iter()
        .find_map(|(k, o)| (k.as_str() == FILES_ENTRY).then_some(*o))
}

/// Every overlap between open items, in `(first, then)` order. `files_text`
/// returns the text of the context record at an offset, if it is inline.
///
/// Pairs already linked either way are left out, since the order is recorded,
/// and so are an item and its own ancestors, since a split shares its parent's files.
pub fn overlaps<'a>(state: &State, files_text: impl Fn(u64) -> Option<&'a str>) -> Vec<Overlap> {
    let declared: Vec<(&Item, u64, Vec<String>)> = state
        .items
        .values()
        .filter(|it| state.stage(&it.id).is_some_and(open))
        .filter_map(|it| {
            let offset = files_offset(it)?;
            let files = parse_files(files_text(offset)?);
            (!files.is_empty()).then_some((it, offset, files))
        })
        .collect();
    let mut out = Vec::new();
    for (i, (a, a_at, a_files)) in declared.iter().enumerate() {
        for (b, b_at, b_files) in &declared[i + 1..] {
            if state.is_linked(&a.id, &b.id)
                || state.is_linked(&b.id, &a.id)
                || descends(state, a, &b.id)
                || descends(state, b, &a.id)
            {
                continue;
            }
            let files = shared(a_files, b_files);
            if files.is_empty() {
                continue;
            }
            let a_first = (rank(state, a), *a_at) <= (rank(state, b), *b_at);
            let (first, then) = if a_first { (a, b) } else { (b, a) };
            out.push(Overlap {
                first: first.id.clone(),
                then: then.id.clone(),
                files,
            });
        }
    }
    out
}

fn open(stage: Stage) -> bool {
    !matches!(stage, Stage::Done | Stage::Closed)
}

/// Lower goes first: work in review or held is further along than work that
/// waits. A tie goes to the item that declared its files first.
fn rank(state: &State, it: &Item) -> u8 {
    match state.stage(&it.id) {
        Some(Stage::InReview) => 0,
        Some(Stage::Held) => 1,
        _ => 2,
    }
}

fn descends(state: &State, it: &Item, ancestor: &ItemId) -> bool {
    let mut parent = it.parent.as_ref();
    while let Some(p) = parent {
        if p == ancestor {
            return true;
        }
        parent = state.items.get(p).and_then(|i| i.parent.as_ref());
    }
    false
}

fn shared(a: &[String], b: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for x in a {
        for y in b {
            if covers(x, y) {
                out.push(y.clone());
            } else if covers(y, x) {
                out.push(x.clone());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Whether `dir` is `path` or a directory above it.
fn covers(dir: &str, path: &str) -> bool {
    dir == path || (dir.ends_with('/') && path.starts_with(dir))
}

#[cfg(test)]
mod tests;
