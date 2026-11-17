//! Time to first human action: how long an item waits on a person before a
//! person writes anything about it. Derived from the records alone, so a
//! replay gives the same numbers. It is a team figure: nothing here names
//! who acted.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use super::{NeedsYou, Stage, State};
use crate::names::ItemId;
use crate::work::{Actor, WorkLogged, WorkRecord};

/// Finished waits kept, oldest dropped first, so a long log costs bounded memory.
const KEPT_WAITS: usize = 20_000;

/// Why an item is waiting on a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitKind {
    /// An agent's proposal or triage answer is waiting to be accepted.
    Proposal,
    /// An agent asked a question, or triage found the issue unclear.
    Question,
    /// A check failed or the attempts are used up.
    Failure,
    /// The work is done and its pull request is in review.
    Review,
    /// Any other reason an item needs you: over budget, left scope, flagged.
    Other,
}

/// One unbroken stretch of waiting. `acted` is set once a person has written
/// a record about the item; the stretch ends when the item stops waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Open {
    kind: WaitKind,
    since_ms: u64,
    acted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Finished {
    kind: WaitKind,
    acted_ms: u64,
    waited_ms: u64,
}

/// Not in the wire form, so a state restored from a snapshot starts with none
/// of it. Equality ignores it for that reason: two states are equal when
/// their wire forms are, whether or not one was restored.
#[derive(Debug, Clone)]
pub(super) struct Waits {
    open: BTreeMap<ItemId, Open>,
    finished: VecDeque<Finished>,
    /// Most waits kept; a field so a test can lower it.
    cap: usize,
    /// When the newest dropped wait ended. Windows reaching back to it are partial.
    dropped_through_ms: Option<u64>,
    /// Items seen closed, so a close frees its dependents once, not on every later record.
    closed: BTreeSet<ItemId>,
    /// Set once a record has been tracked here.
    started: bool,
    /// For a state restored from a snapshot: the time of the first record
    /// tracked after it. Nothing earlier was seen, so windows reaching back
    /// before it are partial.
    restored_from_ms: Option<u64>,
}

impl PartialEq for Waits {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Waits {}

impl Default for Waits {
    fn default() -> Self {
        Self {
            open: BTreeMap::new(),
            finished: VecDeque::new(),
            cap: KEPT_WAITS,
            dropped_through_ms: None,
            closed: BTreeSet::new(),
            started: false,
            restored_from_ms: None,
        }
    }
}

/// Count and percentiles of the waits that ended in a window.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindStats {
    pub count: u64,
    pub p50_ms: Option<u64>,
    pub p90_ms: Option<u64>,
}

/// Time to first human action over a window, for one team.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanWaitStats {
    pub from_ms: u64,
    pub to_ms: u64,
    /// Waits that a person ended inside the window.
    #[serde(flatten)]
    pub all: KindStats,
    pub by_kind: BTreeMap<WaitKind, KindStats>,
    /// Items waiting now with no person action yet. Left out of the percentiles.
    pub open: u64,
    /// When the longest open wait began.
    pub oldest_open_since_ms: Option<u64>,
    /// True when the oldest finished waits were dropped past the cap and the
    /// window reaches back to them, so the figures cover only part of it.
    #[serde(default)]
    pub truncated: bool,
    /// With `truncated`, the first time the figures are complete from. A state
    /// restored from a snapshot is complete only from its first later record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete_since_ms: Option<u64>,
}

pub(super) fn stats(mut ms: Vec<u64>) -> KindStats {
    ms.sort_unstable();
    // Nearest rank: the smallest value with at least p of the waits at or below it.
    let rank = |p: usize| ms.get((ms.len() * p).div_ceil(100).max(1) - 1).copied();
    KindStats {
        count: ms.len() as u64,
        p50_ms: rank(50),
        p90_ms: rank(90),
    }
}

/// An item waiting on a person, as My queue lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueueEntry {
    pub item: ItemId,
    pub kind: WaitKind,
    pub tier: Option<u8>,
    /// When the wait began, if this state saw it begin. A state restored from
    /// a snapshot does not know the start of a wait already open: it began
    /// before the snapshot, so it is older than every known start.
    pub since_ms: Option<u64>,
}

/// Unknown start first, then oldest, then lower tier (untiered last), then id.
pub(super) fn sort_queue(out: &mut [QueueEntry]) {
    out.sort_by(|a, b| {
        a.since_ms
            .cmp(&b.since_ms)
            .then((a.tier.is_none(), a.tier).cmp(&(b.tier.is_none(), b.tier)))
            .then(a.item.cmp(&b.item))
    });
}

impl State {
    /// Every open item that waits on a person: unknown start first (it began
    /// before this state's first record), then oldest wait, then the lower
    /// tier number (untiered last), then id. An item a person snoozed (until
    /// `now_ms`) or parked is left out while the wait they hid continues.
    pub fn queue(&self, now_ms: u64) -> Vec<QueueEntry> {
        let entry = |id: &ItemId| {
            let kind = self.waiting_kind(id)?;
            let it = &self.items[id];
            let since_ms = self.human_waits.open.get(id).map(|o| o.since_ms);
            // A wait that began after the hide is a new reason to look.
            let hidden = |at_ms: u64| since_ms.is_none_or(|s| s <= at_ms);
            let snoozed = it
                .snoozed
                .is_some_and(|z| z.until_ms > now_ms && hidden(z.at_ms));
            if snoozed || it.parked_at_ms.is_some_and(hidden) {
                return None;
            }
            Some(QueueEntry {
                item: id.clone(),
                kind,
                tier: it.tier,
                since_ms,
            })
        };
        // Open waits cover every wait this state saw begin. A restored state
        // also holds waits that began before its snapshot, so it scans.
        let restored = self.human_waits.restored_from_ms.is_some()
            || (!self.human_waits.started && self.at.is_some());
        let mut out: Vec<QueueEntry> = if restored {
            self.items.keys().filter_map(entry).collect()
        } else {
            self.human_waits.open.keys().filter_map(entry).collect()
        };
        sort_queue(&mut out);
        out
    }

    /// Waits a person ended with a record stamped in `from_ms..to_ms`, as
    /// count, p50 and p90 of how long they waited, overall and by reason.
    pub fn time_to_human_action(&self, from_ms: u64, to_ms: u64) -> HumanWaitStats {
        let w = &self.human_waits;
        let hit: Vec<&Finished> = w
            .finished
            .iter()
            .filter(|f| (from_ms..to_ms).contains(&f.acted_ms))
            .collect();
        let mut by_kind = BTreeMap::new();
        for kind in [
            WaitKind::Proposal,
            WaitKind::Question,
            WaitKind::Failure,
            WaitKind::Review,
            WaitKind::Other,
        ] {
            let of = hit.iter().filter(|f| f.kind == kind);
            by_kind.insert(kind, stats(of.map(|f| f.waited_ms).collect()));
        }
        let open = w.open.values().filter(|o| !o.acted);
        // A restored state with no record since has seen nothing: partial, from an unknown time.
        let unseen = !w.started && self.at.is_some();
        let from_dropped = w.dropped_through_ms.map(|d| d.saturating_add(1));
        let complete_since_ms = from_dropped.max(w.restored_from_ms);
        HumanWaitStats {
            from_ms,
            to_ms,
            all: stats(hit.iter().map(|f| f.waited_ms).collect()),
            by_kind,
            open: open.clone().count() as u64,
            oldest_open_since_ms: open.map(|o| o.since_ms).min(),
            truncated: unseen || complete_since_ms.is_some_and(|c| from_ms < c),
            complete_since_ms,
        }
    }

    /// Lowers the cap on finished waits kept.
    #[cfg(test)]
    pub(super) fn keep_waits(&mut self, cap: usize) {
        self.human_waits.cap = cap;
    }

    /// Why `id` waits on a person now, if it does.
    pub(super) fn waiting_kind(&self, id: &ItemId) -> Option<WaitKind> {
        let it = self.items.get(id)?;
        if it.closed.is_some() {
            return None;
        }
        if let Some(why) = self.needs_you(id) {
            return Some(match why {
                NeedsYou::Question | NeedsYou::Unclear => WaitKind::Question,
                NeedsYou::CheckFailed | NeedsYou::TriesUsed | NeedsYou::NoOpenAttempt => {
                    WaitKind::Failure
                }
                _ => WaitKind::Other,
            });
        }
        match self.stage(id)? {
            Stage::Proposed => Some(WaitKind::Proposal),
            Stage::Triage if it.proposal.is_some() => Some(WaitKind::Proposal),
            Stage::InReview => Some(WaitKind::Review),
            _ => None,
        }
    }

    /// Runs after each accepted record. A person's record on a waiting item
    /// ends the wait; then the item's wait state is read again. A snooze or a
    /// park only hides the item from My queue, so it is not an action.
    pub(super) fn track_human_wait(&mut self, rec: &WorkLogged) {
        // `at` is still the previous offset here, so a first record on a state
        // that already has one means the state came from a snapshot.
        if !self.human_waits.started {
            self.human_waits.started = true;
            if self.at.is_some() {
                self.human_waits.restored_from_ms = Some(rec.at_ms);
            }
        }
        let Some(id) = rec.record.item().cloned() else {
            return;
        };
        let hides = matches!(
            rec.record,
            WorkRecord::Snoozed { .. } | WorkRecord::Parked { .. }
        );
        if rec.actor == Actor::Person
            && !hides
            && let Some(o) = self.human_waits.open.get_mut(&id)
            && !o.acted
        {
            o.acted = true;
            let done = Finished {
                kind: o.kind,
                acted_ms: rec.at_ms,
                waited_ms: rec.at_ms.saturating_sub(o.since_ms),
            };
            let fin = &mut self.human_waits.finished;
            if fin.len() >= self.human_waits.cap
                && let Some(old) = fin.pop_front()
            {
                let d = &mut self.human_waits.dropped_through_ms;
                *d = (*d).max(Some(old.acted_ms));
            }
            fin.push_back(done);
        }
        self.refresh_wait(&id, rec.at_ms);
        // Closing an item can free or drop what waits on it.
        let closed = self.items.get(&id).is_some_and(|i| i.closed.is_some());
        let newly = if closed {
            self.human_waits.closed.insert(id.clone())
        } else {
            self.human_waits.closed.remove(&id);
            false
        };
        if newly {
            let dependents: Vec<ItemId> = self
                .edges
                .iter()
                .filter(|(_, b)| *b == id)
                .map(|(a, _)| a.clone())
                .collect();
            for a in dependents {
                self.refresh_wait(&a, rec.at_ms);
            }
        }
    }

    fn refresh_wait(&mut self, id: &ItemId, at_ms: u64) {
        let now = self.waiting_kind(id);
        let open = &mut self.human_waits.open;
        match (open.contains_key(id), now) {
            (false, Some(kind)) => {
                open.insert(
                    id.clone(),
                    Open {
                        kind,
                        since_ms: at_ms,
                        acted: false,
                    },
                );
            }
            (true, None) => {
                open.remove(id);
            }
            // A different reason is a new wait, so it ages from this record.
            (true, Some(kind)) if open.get(id).is_some_and(|o| o.kind != kind) => {
                open.insert(
                    id.clone(),
                    Open {
                        kind,
                        since_ms: at_ms,
                        acted: false,
                    },
                );
            }
            _ => {}
        }
    }
}
