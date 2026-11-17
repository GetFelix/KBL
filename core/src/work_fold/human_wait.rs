//! Time to first human action: how long an item waits on a person before a
//! person writes anything about it. Derived from the records alone, so a
//! replay gives the same numbers. It is a team figure: nothing here names
//! who acted.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use super::{NeedsYou, Stage, State};
use crate::names::ItemId;
use crate::work::{Actor, WorkLogged};

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

#[derive(Debug, Clone, Default)]
pub(super) struct Waits {
    open: BTreeMap<ItemId, Open>,
    finished: VecDeque<Finished>,
}

/// Never part of a state's identity: it is not in the wire form, so a state
/// read back from JSON must still equal the one that was written.
impl PartialEq for Waits {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for Waits {}

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

impl State {
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
        HumanWaitStats {
            from_ms,
            to_ms,
            all: stats(hit.iter().map(|f| f.waited_ms).collect()),
            by_kind,
            open: open.clone().count() as u64,
            oldest_open_since_ms: open.map(|o| o.since_ms).min(),
        }
    }

    /// Why `id` waits on a person now, if it does.
    fn waiting_kind(&self, id: &ItemId) -> Option<WaitKind> {
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
    /// ends the wait; then the item's wait state is read again. A snooze is
    /// an unknown kind to this build, which names no item, so it is not an action.
    pub(super) fn track_human_wait(&mut self, rec: &WorkLogged) {
        let Some(id) = rec.record.item().cloned() else {
            return;
        };
        if rec.actor == Actor::Person
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
            if fin.len() == KEPT_WAITS {
                fin.pop_front();
            }
            fin.push_back(done);
        }
        self.refresh_wait(&id, rec.at_ms);
        // Closing an item can free or drop what waits on it.
        if self.items.get(&id).is_some_and(|i| i.closed.is_some()) {
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
            _ => {}
        }
    }
}
