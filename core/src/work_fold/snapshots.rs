//! Folding to an offset, and the snapshots that make it cheap.
//!
//! A snapshot is a cache of the fold, never a source of truth: the log can
//! always rebuild it. One is used only after it is checked against the log it
//! would stand in for, and a snapshot that fails is skipped, so a stale or
//! foreign one costs a longer fold and never a wrong state.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::human_wait::Waits;
use super::trust::MarkTimes;
use super::{State, Waiting, fold};
use crate::artifact::ArtifactId;
use crate::codec::Codec;
use crate::names::{ItemId, Key};
use crate::work::WorkLogged;

/// Records between snapshots. Counted in records applied, not offsets, so
/// the blank offsets of a Raft shard do not move them.
pub const SNAPSHOT_EVERY: u64 = 2_000;

/// Bump when a change to the fold would make an old snapshot differ from a
/// fold of the same prefix. Snapshots from another revision are dropped.
pub const FOLD_REV: u32 = 4;

/// Every record at an offset up to and including `upto`.
///
/// Offsets that hold no record (a Raft shard burns one on a refused
/// conditional publish) are simply absent from `log`, so the state at a blank
/// is the state at the last record before it. Offsets must ascend.
pub fn fold_to<'a>(log: impl IntoIterator<Item = &'a (u64, WorkLogged)>, upto: u64) -> State {
    fold(log.into_iter().take_while(|(o, _)| *o <= upto))
}

/// The state after the record at `through`, with what is needed to tell
/// whether it still describes the log.
///
/// `state` has no wire form for the proposals waiting on a person or the
/// human-wait record, so the snapshot keeps them beside it and puts them back
/// in [`Snapshot::state`]. Replaying them would need the records the snapshot
/// exists to skip, so only keeping them restores a state exactly equal to a fold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub rev: u32,
    /// Offset of the last record folded in.
    pub through: u64,
    /// Records folded in, which is the index of the next one.
    pub records: u64,
    /// Hash over every record folded in, chained from the previous
    /// snapshot's, so an edit anywhere in the prefix changes it.
    pub chain: String,
    pub(in crate::work_fold) state: State,
    waiting: Waiting,
    human_waits: Waits,
    mark_times: MarkTimes,
    /// What the state's wire form leaves out of each item.
    #[serde(default)]
    left_out: BTreeMap<ItemId, LeftOut>,
}

/// The item fields `State` does not serialize, which a fold of the log sets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LeftOut {
    proposed_from: Option<(Key, ItemId)>,
    proposal_by: Option<Key>,
}

impl Snapshot {
    fn of(through: u64, records: u64, chain: String, state: &State) -> Self {
        let mut state = state.clone();
        let left_out = (state.items.iter())
            .filter_map(|(id, i)| {
                let proposal_by = i.proposal.as_ref().and_then(|p| p.by.clone());
                (i.proposed_from.is_some() || proposal_by.is_some()).then(|| {
                    let left = LeftOut {
                        proposed_from: i.proposed_from.clone(),
                        proposal_by,
                    };
                    (id.clone(), left)
                })
            })
            .collect();
        Self {
            left_out,
            rev: FOLD_REV,
            through,
            records,
            chain,
            waiting: std::mem::take(&mut state.waiting),
            human_waits: std::mem::take(&mut state.human_waits),
            mark_times: std::mem::take(&mut state.mark_times),
            state,
        }
    }

    /// The state this snapshot holds, as a fold of the same prefix would.
    pub fn state(&self) -> State {
        let mut state = self.state.clone();
        state.waiting = self.waiting.clone();
        state.human_waits = self.human_waits.clone();
        state.mark_times = self.mark_times.clone();
        for (id, left) in &self.left_out {
            if let Some(item) = state.items.get_mut(id) {
                item.proposed_from.clone_from(&left.proposed_from);
                if let Some(p) = &mut item.proposal {
                    p.by.clone_from(&left.proposal_by);
                }
            }
        }
        state
    }

    /// Whether this snapshot is the fold of `log` up to `through`, given that
    /// the one before it (`prev_chain`, `prev_records`) is: same revision, the
    /// chain over the records since the previous snapshot matches, and the
    /// state ends where it says. Costs only those records.
    fn describes(&self, log: &[(u64, WorkLogged)], prev_chain: &str, prev_records: u64) -> bool {
        let (Ok(from), Ok(to)) = (usize::try_from(prev_records), usize::try_from(self.records))
        else {
            return false;
        };
        self.rev == FOLD_REV
            && self.state.at == Some(self.through)
            && from < to
            && log.get(to - 1).is_some_and(|(o, _)| *o == self.through)
            && link(prev_chain, &log[from..to]) == self.chain
    }
}

/// The chain after `records`, which follow a prefix whose chain is `prev`.
fn link(prev: &str, records: &[(u64, WorkLogged)]) -> String {
    let mut bytes = prev.as_bytes().to_vec();
    for (offset, rec) in records {
        let enc = rec.encode();
        bytes.extend(offset.to_be_bytes());
        bytes.extend((enc.len() as u64).to_be_bytes());
        bytes.extend(enc);
    }
    ArtifactId::of(&bytes).as_str().to_owned()
}

/// Where a saved snapshot's chain stood before it: the chain over the first
/// `records` records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Link {
    records: u64,
    chain: String,
}

/// How far a hash chain has been taken: the links before it, its head and the
/// records it covers. A running tracker keeps this instead of the log, and
/// extends it with the records applied since, so a save hashes only those.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Anchor {
    links: Vec<Link>,
    chain: String,
    records: u64,
}

impl Anchor {
    /// Records of the log the chain covers.
    pub fn records(&self) -> u64 {
        self.records
    }

    /// The snapshot of the log up to the last of `tail`, the records that
    /// follow this anchor, whose fold is `state`. `None` if `tail` is empty or
    /// `state` does not end at its last record.
    pub fn extend(&self, tail: &[(u64, WorkLogged)], state: &State) -> Option<Saved> {
        let (last, _) = tail.last()?;
        if state.at != Some(*last) {
            return None;
        }
        let mut links = self.links.clone();
        if self.records > 0 {
            links.push(Link {
                records: self.records,
                chain: self.chain.clone(),
            });
        }
        let records = self.records + tail.len() as u64;
        let chain = link(&self.chain, tail);
        Some(Saved {
            links,
            snapshot: Snapshot::of(*last, records, chain, state),
        })
    }
}

/// One snapshot kept on its own, for a store that holds only the newest.
///
/// Its chain is built over the links before it, so [`Saved::valid`] hashes
/// every record once and needs no older snapshot. The state is not covered by
/// the chain: the store must catch a damaged file itself, and the chain only
/// decides whether the log still matches what the file was built from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    links: Vec<Link>,
    snapshot: Snapshot,
}

impl Saved {
    /// A snapshot of all of `log`, whose fold is `state`. `prev` is the saved
    /// snapshot `log` was checked against, if one was.
    pub fn capture(log: &[(u64, WorkLogged)], prev: Option<&Saved>, state: &State) -> Option<Self> {
        let anchor = prev.map_or_else(Anchor::default, Saved::anchor);
        let from = usize::try_from(anchor.records).ok()?;
        anchor.extend(log.get(from..)?, state)
    }

    /// Where the chain stands after this snapshot, for extending it without
    /// the log.
    pub fn anchor(&self) -> Anchor {
        Anchor {
            links: self.links.clone(),
            chain: self.snapshot.chain.clone(),
            records: self.snapshot.records,
        }
    }

    /// Whether this is the fold of the first [`Saved::records`] records of `log`.
    pub fn valid(&self, log: &[(u64, WorkLogged)]) -> bool {
        let (mut chain, mut records) = (String::new(), 0u64);
        for l in &self.links {
            let (Ok(from), Ok(to)) = (usize::try_from(records), usize::try_from(l.records)) else {
                return false;
            };
            if from >= to || to > log.len() || link(&chain, &log[from..to]) != l.chain {
                return false;
            }
            chain.clone_from(&l.chain);
            records = l.records;
        }
        self.snapshot.describes(log, &chain, records)
    }

    /// How many records of the log this covers.
    pub fn records(&self) -> u64 {
        self.snapshot.records
    }

    /// The state, as a fold of the same records gives it.
    pub fn state(&self) -> State {
        self.snapshot.state()
    }
}

/// The chain points of a saved snapshot, read from a file without its state,
/// so a file can be told apart from the log without loading it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "LineageFile")]
pub struct Lineage {
    /// `(records, chain)` oldest first, ending at the snapshot itself.
    points: Vec<(u64, String)>,
    /// Offset of the last record the snapshot folded in.
    through: u64,
}

#[derive(Deserialize)]
struct LineageFile {
    links: Vec<Link>,
    snapshot: LineageHead,
}

#[derive(Deserialize)]
struct LineageHead {
    through: u64,
    records: u64,
    chain: String,
}

impl From<LineageFile> for Lineage {
    fn from(f: LineageFile) -> Self {
        let mut points: Vec<_> = f.links.into_iter().map(|l| (l.records, l.chain)).collect();
        points.push((f.snapshot.records, f.snapshot.chain));
        Self {
            points,
            through: f.snapshot.through,
        }
    }
}

impl Lineage {
    /// Whether `log` contradicts this snapshot: a point it covers whose chain
    /// differs from the log's. After retention trims the head of the log, every
    /// older file does.
    ///
    /// A snapshot with more records than `log` may only be a short read of
    /// the same log, which the log will outgrow. The two are told apart by
    /// offsets: offsets ascend, so a short prefix ends before the snapshot's
    /// last offset, and a log that already holds an offset at or past it with
    /// fewer records has lost its head. That holds even when no chain point of
    /// the snapshot falls inside the log.
    pub fn diverges_from(&self, log: &[(u64, WorkLogged)]) -> bool {
        let (mut chain, mut done) = (String::new(), 0usize);
        for (records, want) in &self.points {
            let Ok(to) = usize::try_from(*records) else {
                return false;
            };
            if to > log.len() {
                return log.last().is_some_and(|(o, _)| *o >= self.through);
            }
            if to <= done {
                return true;
            }
            chain = link(&chain, &log[done..to]);
            done = to;
            if chain != *want {
                return true;
            }
        }
        false
    }

    /// Whether every point of this snapshot lies inside `log`. Together with
    /// [`Lineage::diverges_from`] returning false, that means `log` has been
    /// checked against all of it.
    pub fn within(&self, log: &[(u64, WorkLogged)]) -> bool {
        self.points
            .last()
            .is_some_and(|(r, _)| usize::try_from(*r).is_ok_and(|r| r <= log.len()))
    }
}

/// Snapshots of one log, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshots {
    snaps: Vec<Snapshot>,
}

impl Snapshots {
    /// One snapshot every [`SNAPSHOT_EVERY`] records of `log`.
    pub fn build(log: &[(u64, WorkLogged)]) -> Self {
        Self::build_every(log, SNAPSHOT_EVERY)
    }

    /// As [`Snapshots::build`] with another spacing. A spacing of 0 makes none.
    pub fn build_every(log: &[(u64, WorkLogged)], every: u64) -> Self {
        let mut s = Self::default();
        s.extend(log, every, State::default(), 0, String::new());
        s
    }

    /// Snapshots read from storage. Nothing about them is trusted until used.
    pub fn from_stored(snaps: Vec<Snapshot>) -> Self {
        Self { snaps }
    }

    pub fn as_slice(&self) -> &[Snapshot] {
        &self.snaps
    }

    /// How many leading snapshots describe `log`, each checked against the
    /// one before it.
    fn good_prefix(&self, log: &[(u64, WorkLogged)]) -> usize {
        let (mut chain, mut records) = (String::new(), 0);
        self.snaps
            .iter()
            .take_while(|s| {
                let ok = s.describes(log, &chain, records);
                chain.clone_from(&s.chain);
                records = s.records;
                ok
            })
            .count()
    }

    /// Drops the snapshots that do not describe `log` and every one after
    /// them, then builds on from the last good one, so a log that grew gets
    /// its new snapshots and a stale tail is rebuilt.
    pub fn refresh(&mut self, log: &[(u64, WorkLogged)]) {
        self.refresh_every(log, SNAPSHOT_EVERY);
    }

    pub fn refresh_every(&mut self, log: &[(u64, WorkLogged)], every: u64) {
        let good = self.good_prefix(log);
        self.snaps.truncate(good);
        match self.snaps.last() {
            Some(s) => {
                let (state, from, chain) = (s.state(), s.records, s.chain.clone());
                self.extend(log, every, state, from, chain);
            }
            None => self.extend(log, every, State::default(), 0, String::new()),
        }
    }

    fn extend(
        &mut self,
        log: &[(u64, WorkLogged)],
        every: u64,
        mut state: State,
        from: u64,
        mut chain: String,
    ) {
        if every == 0 {
            return;
        }
        let from = usize::try_from(from).expect("a log fits in memory");
        let mut since = from;
        for (i, (offset, rec)) in log.iter().enumerate().skip(from) {
            state.apply(*offset, rec);
            let records = i as u64 + 1;
            if records.is_multiple_of(every) {
                chain = link(&chain, &log[since..=i]);
                since = i + 1;
                self.snaps
                    .push(Snapshot::of(*offset, records, chain.clone(), &state));
            }
        }
    }

    /// The state at `upto`, the same as [`fold_to`] over `log`, starting from
    /// the newest snapshot at or before `upto` that still describes `log`.
    /// Checking one hashes the records before it, which costs far less than
    /// folding them.
    pub fn fold_to(&self, log: &[(u64, WorkLogged)], upto: u64) -> State {
        let good = &self.snaps[..self.good_prefix(log)];
        let usable = good.iter().rev().find(|s| s.through <= upto);
        let (mut state, from) = match usable {
            Some(s) => (s.state(), s.records as usize),
            None => (State::default(), 0),
        };
        for (offset, rec) in &log[from..] {
            if *offset > upto {
                break;
            }
            state.apply(*offset, rec);
        }
        state
    }
}
