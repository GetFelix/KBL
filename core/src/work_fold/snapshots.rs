//! Folding to an offset, and the snapshots that make it cheap.
//!
//! A snapshot is a cache of the fold, never a source of truth: the log can
//! always rebuild it. One is used only after it is checked against the log it
//! would stand in for, and a snapshot that fails is skipped, so a stale or
//! foreign one costs a longer fold and never a wrong state.

use serde::{Deserialize, Serialize};

use super::{State, fold};
use crate::artifact::ArtifactId;
use crate::codec::Codec;
use crate::work::WorkLogged;

/// Records between snapshots. Counted in records applied, not offsets, so
/// the blank offsets of a Raft shard do not move them.
pub const SNAPSHOT_EVERY: u64 = 2_000;

/// Bump when a change to the fold would make an old snapshot differ from a
/// fold of the same prefix. Snapshots from another revision are dropped.
pub const FOLD_REV: u32 = 1;

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub rev: u32,
    /// Offset of the last record folded in.
    pub through: u64,
    /// Records folded in, which is the index of the next one.
    pub records: u64,
    /// Hash of the record at `through` and its offset.
    pub tail: String,
    pub state: State,
}

impl Snapshot {
    fn of(through: u64, records: u64, rec: &WorkLogged, state: &State) -> Self {
        Self {
            rev: FOLD_REV,
            through,
            records,
            tail: tail_hash(through, rec),
            state: state.clone(),
        }
    }

    /// Whether this snapshot is the fold of `log` up to `through`: same
    /// revision, the record count and tail record match, and the state ends
    /// where it says. A log rewritten or trimmed below `through` fails.
    fn describes(&self, log: &[(u64, WorkLogged)]) -> bool {
        let Some(i) = usize::try_from(self.records)
            .ok()
            .and_then(|n| n.checked_sub(1))
        else {
            return false;
        };
        self.rev == FOLD_REV
            && self.state.at == Some(self.through)
            && log
                .get(i)
                .is_some_and(|(o, rec)| *o == self.through && tail_hash(*o, rec) == self.tail)
    }
}

fn tail_hash(offset: u64, rec: &WorkLogged) -> String {
    let mut bytes = offset.to_be_bytes().to_vec();
    bytes.extend(rec.encode());
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
        s.extend(log, every, State::default(), 0);
        s
    }

    /// Snapshots read from storage. Nothing about them is trusted until used.
    pub fn from_stored(snaps: Vec<Snapshot>) -> Self {
        Self { snaps }
    }

    pub fn as_slice(&self) -> &[Snapshot] {
        &self.snaps
    }

    /// Drops the snapshots that do not describe `log` and every one after
    /// them, then builds on from the last good one, so a log that grew gets
    /// its new snapshots and a stale tail is rebuilt.
    pub fn refresh(&mut self, log: &[(u64, WorkLogged)]) {
        self.refresh_every(log, SNAPSHOT_EVERY);
    }

    pub fn refresh_every(&mut self, log: &[(u64, WorkLogged)], every: u64) {
        let good = self
            .snaps
            .iter()
            .enumerate()
            .take_while(|(i, s)| {
                s.describes(log) && (*i == 0 || self.snaps[*i - 1].records < s.records)
            })
            .count();
        self.snaps.truncate(good);
        let (state, from) = match self.snaps.last() {
            Some(s) => (s.state.clone(), s.records),
            None => (State::default(), 0),
        };
        self.extend(log, every, state, from);
    }

    fn extend(&mut self, log: &[(u64, WorkLogged)], every: u64, mut state: State, from: u64) {
        if every == 0 {
            return;
        }
        let from = usize::try_from(from).expect("a log fits in memory");
        for (i, (offset, rec)) in log.iter().enumerate().skip(from) {
            state.apply(*offset, rec);
            let records = i as u64 + 1;
            if records.is_multiple_of(every) {
                self.snaps.push(Snapshot::of(*offset, records, rec, &state));
            }
        }
    }

    /// The state at `upto`, the same as [`fold_to`] over `log`, starting from
    /// the newest snapshot at or before `upto` that still describes `log`.
    pub fn fold_to(&self, log: &[(u64, WorkLogged)], upto: u64) -> State {
        let usable = self
            .snaps
            .iter()
            .rev()
            .find(|s| s.through <= upto && s.describes(log));
        let (mut state, from) = match usable {
            Some(s) => (s.state.clone(), s.records as usize),
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
