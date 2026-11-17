//! Folding to an offset, and the snapshots that make it cheap.
//!
//! A snapshot is a cache of the fold, never a source of truth: the log can
//! always rebuild it. One is used only after it is checked against the log it
//! would stand in for, and a snapshot that fails is skipped, so a stale or
//! foreign one costs a longer fold and never a wrong state.

use serde::{Deserialize, Serialize};

use super::human_wait::Waits;
use super::{State, Waiting, fold};
use crate::artifact::ArtifactId;
use crate::codec::Codec;
use crate::work::WorkLogged;

/// Records between snapshots. Counted in records applied, not offsets, so
/// the blank offsets of a Raft shard do not move them.
pub const SNAPSHOT_EVERY: u64 = 2_000;

/// Bump when a change to the fold would make an old snapshot differ from a
/// fold of the same prefix. Snapshots from another revision are dropped.
pub const FOLD_REV: u32 = 2;

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
}

impl Snapshot {
    fn of(through: u64, records: u64, chain: String, state: &State) -> Self {
        let mut state = state.clone();
        Self {
            rev: FOLD_REV,
            through,
            records,
            chain,
            waiting: std::mem::take(&mut state.waiting),
            human_waits: std::mem::take(&mut state.human_waits),
            state,
        }
    }

    /// The state this snapshot holds, as a fold of the same prefix would.
    pub fn state(&self) -> State {
        let mut state = self.state.clone();
        state.waiting = self.waiting.clone();
        state.human_waits = self.human_waits.clone();
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
