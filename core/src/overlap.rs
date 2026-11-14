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
use crate::work::Target;
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

/// A path that stands for the whole repo: `./`, `/` or `.` in an entry.
/// It overlaps every path declared for the same repo.
pub const WHOLE_REPO: &str = "./";

/// The paths in a `files` entry, one per line, normalised: surrounding space
/// trimmed, `\\` read as `/`, leading `/` and `./` and repeated `/` dropped,
/// and `..` resolved. A line whose `..` climbs above the repo is skipped, as
/// are blank lines and `#` comments. A path ending in `/` is a directory and
/// covers everything under it. Paths compare byte for byte: no case folding
/// and no globs.
pub fn parse_files(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(normalise)
        .collect();
    out.sort();
    out.dedup();
    out
}

fn normalise(line: &str) -> Option<String> {
    let line = line.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for part in line.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Some(WHOLE_REPO.to_owned());
    }
    let mut path = parts.join("/");
    if line.ends_with('/') {
        path.push('/');
    }
    Some(path)
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
/// Only items whose roots land in the same repo are compared; items with no
/// target are compared with each other. A pair is left out when either item
/// already reaches the other along links, since the fold would refuse the
/// suggested link or the order is recorded, and so is an item and its own
/// ancestors, since a split shares its parent's files.
///
/// The cost is pairwise: every two declaring items in a repo, and every two of
/// their paths.
pub fn overlaps<'a>(state: &State, files_text: impl Fn(u64) -> Option<&'a str>) -> Vec<Overlap> {
    let declared: Vec<(&Item, Option<&Target>, u64, Vec<String>)> = state
        .items
        .values()
        .filter(|it| state.stage(&it.id).is_some_and(open))
        .filter_map(|it| {
            let offset = files_offset(it)?;
            let files = parse_files(files_text(offset)?);
            (!files.is_empty()).then_some((it, root_target(state, it), offset, files))
        })
        .collect();
    let mut out = Vec::new();
    for (i, (a, a_repo, a_at, a_files)) in declared.iter().enumerate() {
        for (b, b_repo, b_at, b_files) in &declared[i + 1..] {
            if a_repo != b_repo
                || state.reaches(&a.id, &b.id)
                || state.reaches(&b.id, &a.id)
                || descends(state, a, &b.id)
                || descends(state, b, &a.id)
            {
                continue;
            }
            let files = shared(a_files, b_files);
            if files.is_empty() {
                continue;
            }
            let a_first = (rank(a), *a_at) <= (rank(b), *b_at);
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

/// Lower goes first: held work, then work in review, then work nobody has
/// started. This reads the hold, not the stage, so a held item with an open
/// question still counts as held. A tie goes to the item whose current
/// `files` entry is older.
fn rank(it: &Item) -> u8 {
    if it.hold.is_some() {
        0
    } else if it.pending_fork.is_none() && it.open_path().is_some_and(|p| p.done) {
        1
    } else {
        2
    }
}

/// The target of the item's root, which is where the item's work lands.
fn root_target<'s>(state: &'s State, it: &'s Item) -> Option<&'s Target> {
    let mut root = it;
    while let Some(p) = root.parent.as_ref().and_then(|p| state.items.get(p)) {
        root = p;
    }
    root.target.as_ref()
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
    dir == WHOLE_REPO || dir == path || (dir.ends_with('/') && path.starts_with(dir))
}

#[cfg(test)]
mod tests;
