//! Flow, rework, cost and supervision numbers for the Metrics dialog.
//!
//! Derived from the records the fold accepted and the state it holds; nothing is added to the log.
//! A record the fold refused, and a `refused` record the tracker logged, count nowhere. The numbers
//! are team figures: nothing here names an agent, a model's user or a person. See docs/metrics.md.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use kbl_core::names::ItemId;
use kbl_core::work::{Actor, ReleaseWhy, WorkLogged, WorkRecord};
use kbl_core::work_fold::{Stage, State};
use serde_json::{Value, json};

const DAY_MS: u64 = 86_400_000;
/// Longest window the report answers for.
const MAX_DAYS: u32 = 365;
/// Finished stage visits kept, oldest dropped first, so a long log costs bounded memory.
const KEPT_VISITS: usize = 50_000;

/// The stages an item can wait in. Done and Closed end the item and have no duration.
const TIMED: [Stage; 7] = [
    Stage::Proposed,
    Stage::Triage,
    Stage::NeedsYou,
    Stage::Blocked,
    Stage::Ready,
    Stage::Held,
    Stage::InReview,
];

/// One finished stay in a stage.
struct Visit {
    stage: Stage,
    left_ms: u64,
    ms: u64,
}

#[derive(Default)]
struct Facts {
    /// The stage the item is in and since when.
    now: Option<(Stage, u64)>,
    /// Records a person wrote about it that the fold took.
    touches: u64,
    failed_attempts: u64,
    reopened: u64,
    /// Time spent waiting on a person (Needs you or In review), over its life.
    wait_ms: u64,
    /// When it last entered Done; none while it is not done.
    done_ms: Option<u64>,
}

#[derive(Default)]
pub(crate) struct Metrics {
    facts: BTreeMap<ItemId, Facts>,
    visits: VecDeque<Visit>,
    /// Items in Blocked now, which a record on any tree can free.
    blocked: BTreeSet<ItemId>,
    /// When the newest dropped visit ended. Windows reaching back to it are partial.
    dropped_through_ms: Option<u64>,
    /// The newest record time seen, to tell a browser clock that lags the log.
    latest_ms: u64,
}

/// Nearest-rank percentile of sorted `v`.
fn pct(v: &[u64], p: usize) -> Option<u64> {
    if v.is_empty() {
        return None;
    }
    let rank = (p * v.len()).div_ceil(100).max(1);
    Some(v[rank - 1])
}

fn spread(mut v: Vec<u64>) -> Value {
    v.sort_unstable();
    json!({ "n": v.len(), "p50_ms": pct(&v, 50), "p85_ms": pct(&v, 85) })
}

/// Records whose effect on stages stops at the item and its ancestors.
fn own_stage_only(r: &WorkRecord) -> bool {
    matches!(
        r,
        WorkRecord::Claimed { .. }
            | WorkRecord::Released { .. }
            | WorkRecord::GaveUp { .. }
            | WorkRecord::Done { .. }
            | WorkRecord::Progress { .. }
            | WorkRecord::PrStatus { .. }
            | WorkRecord::Snoozed { .. }
            | WorkRecord::Parked { .. }
            | WorkRecord::SummaryRated { .. }
    )
}

/// Records that cannot move any item's stage.
fn keeps_stage(r: &WorkRecord) -> bool {
    matches!(
        r,
        WorkRecord::Progress { pr: None, .. }
            | WorkRecord::Step { .. }
            | WorkRecord::Edit { .. }
            | WorkRecord::Decision { .. }
            | WorkRecord::Context { .. }
    )
}

impl Metrics {
    /// Notes `rec`, applied to `state` just now. `refused` is the fold's verdict on it.
    pub(crate) fn note(&mut self, state: &State, rec: &WorkLogged, refused: bool) {
        if refused || matches!(rec.record, WorkRecord::Unknown) {
            return;
        }
        let Some(item) = rec.record.item() else {
            return;
        };
        if !state.items.contains_key(item) {
            return;
        }
        self.latest_ms = self.latest_ms.max(rec.at_ms);
        // The tracker's logged refusal can still move a stage (a budget refusal sends the root to
        // Needs you), but it is no work, so it adds to no count.
        if !matches!(rec.record, WorkRecord::Refused { .. }) {
            let f = self.facts.entry(item.clone()).or_default();
            if rec.actor == Actor::Person {
                f.touches += 1;
            }
            match &rec.record {
                WorkRecord::Released { why, .. } if why.is_none_or(|w| w == ReleaseWhy::Failed) => {
                    f.failed_attempts += 1;
                }
                WorkRecord::GaveUp { .. } => f.failed_attempts += 1,
                WorkRecord::Reopened { .. } => f.reopened += 1,
                _ => {}
            }
        }
        if keeps_stage(&rec.record) {
            return;
        }
        let mut ids: Vec<ItemId> = Vec::new();
        if own_stage_only(&rec.record) {
            // The item and its ancestors, whose stage reads their children.
            let mut at = Some(item.clone());
            while let Some(id) = at {
                at = state.items.get(&id).and_then(|i| i.parent.clone());
                ids.push(id);
            }
        } else {
            let mut stack = vec![state.items[item].root.clone()];
            while let Some(id) = stack.pop() {
                if let Some(it) = state.items.get(&id) {
                    stack.extend(it.children.iter().cloned());
                    ids.push(id);
                }
            }
        }
        // A link holds across trees, so a close in one can free a blocked item in another.
        ids.extend(self.blocked.iter().cloned());
        for id in ids {
            self.enter(&id, state.stage(&id), rec.at_ms);
        }
    }

    fn enter(&mut self, id: &ItemId, stage: Option<Stage>, at_ms: u64) {
        let Some(stage) = stage else { return };
        let f = self.facts.entry(id.clone()).or_default();
        if f.now.is_some_and(|(s, _)| s == stage) {
            return;
        }
        if let Some((old, since)) = f.now {
            // A clock that steps back across a restart gives a zero stay, not a huge one.
            let ms = at_ms.saturating_sub(since);
            if TIMED.contains(&old) {
                if matches!(old, Stage::NeedsYou | Stage::InReview) {
                    f.wait_ms = f.wait_ms.saturating_add(ms);
                }
                self.visits.push_back(Visit {
                    stage: old,
                    left_ms: at_ms.max(since),
                    ms,
                });
                while self.visits.len() > KEPT_VISITS {
                    let gone = self.visits.pop_front().expect("over the cap, so not empty");
                    self.dropped_through_ms = self.dropped_through_ms.max(Some(gone.left_ms));
                }
            }
        }
        if stage == Stage::Blocked {
            self.blocked.insert(id.clone());
        } else {
            self.blocked.remove(id);
        }
        let f = self.facts.get_mut(id).expect("inserted above");
        // A record stamped before the stay began does not move its start back.
        let since = f.now.map_or(at_ms, |(_, t)| t.max(at_ms));
        f.now = Some((stage, since));
        f.done_ms = (stage == Stage::Done).then_some(since);
    }

    /// The report over the `days` before `now_ms`: stage times by when each stay ended, and the
    /// items that are done now by when they last became done. An item reopened since is not done,
    /// so a redo counts once.
    ///
    /// A clock behind the newest record would drop the latest stays from the window, so `now_ms`
    /// is raised to that record and `clock_behind` says so.
    pub(crate) fn report(&self, state: &State, now_ms: u64, days: u32) -> Value {
        let clock_behind = now_ms < self.latest_ms;
        let now_ms = now_ms.max(self.latest_ms);
        let days = days.clamp(1, MAX_DAYS);
        let from = now_ms.saturating_sub(u64::from(days) * DAY_MS);
        let in_window = |t: u64| t > from && t <= now_ms;

        let stages: Vec<Value> = TIMED
            .iter()
            .map(|&s| {
                let v: Vec<u64> = self
                    .visits
                    .iter()
                    .filter(|x| x.stage == s && in_window(x.left_ms))
                    .map(|x| x.ms)
                    .collect();
                let mut o = spread(v);
                o["stage"] = json!(s);
                o
            })
            .collect();

        let done: Vec<(&ItemId, &Facts, u64)> = self
            .facts
            .iter()
            .filter_map(|(id, f)| {
                let at = f.done_ms?;
                (in_window(at) && state.stage(id) == Some(Stage::Done)).then_some((id, f, at))
            })
            .collect();
        let n = done.len() as u64;

        let mut per_day = vec![0u64; days as usize];
        for (_, _, at) in &done {
            // Day 0 is the one ending at `now_ms`; the list is oldest first.
            let back = (now_ms - at) / DAY_MS;
            per_day[days as usize - 1 - back as usize] += 1;
        }
        // Weeks are counted back from now, so the oldest may be a part week.
        let mut per_week: Vec<u64> = per_day.rchunks(7).map(|w| w.iter().sum()).collect();
        per_week.reverse();

        let sum =
            |f: &dyn Fn(&ItemId, &Facts) -> u64| done.iter().map(|(i, x, _)| f(i, x)).sum::<u64>();
        let redos = sum(&|i, _| state.items.get(i).map_or(0, |x| u64::from(x.redos)));
        let reopened = sum(&|_, f| f.reopened);
        let failed = sum(&|_, f| f.failed_attempts);

        let cost_total = sum(&|i, _| state.items.get(i).map_or(0, |x| x.cost_micros));
        let mut by_model: BTreeMap<&str, (u64, bool)> = BTreeMap::new();
        for (i, _, _) in &done {
            for (m, u) in state.items.get(*i).iter().flat_map(|x| x.usage.iter()) {
                let e = by_model.entry(m.as_str()).or_default();
                e.0 = e.0.saturating_add(u.cost_micros);
                e.1 |= u.unpriced;
            }
        }
        let attributed: u64 = by_model.values().map(|(c, _)| *c).sum();
        let per = |total: u64| (n > 0).then(|| total / n);
        let ratio = |x: u64| (n > 0).then(|| x as f64 / n as f64);

        let touched = done.iter().filter(|(_, f, _)| f.touches > 0).count() as u64;
        let touches = sum(&|_, f| f.touches);
        let waits: Vec<u64> = done.iter().map(|(_, f, _)| f.wait_ms).collect();

        json!({
            "days": days,
            "from_ms": from,
            "to_ms": now_ms,
            "clock_behind": clock_behind,
            "partial": self.dropped_through_ms.is_some_and(|t| t > from),
            "stages": stages,
            "throughput": { "done": n, "per_day": per_day, "per_week": per_week },
            "rework": {
                "done": n,
                "redos": redos,
                "reopened": reopened,
                "failed_attempts": failed,
                "redos_per_done": ratio(redos),
                "reopened_per_done": ratio(reopened),
                "failed_attempts_per_done": ratio(failed),
            },
            "cost": {
                "done": n,
                "total_micros": cost_total,
                "per_done_micros": per(cost_total),
                "unattributed_micros": cost_total.saturating_sub(attributed),
                "by_model": by_model.iter().map(|(m, (c, unpriced))| json!({
                    "model": m,
                    "cost_micros": c,
                    "per_done_micros": per(*c),
                    "unpriced": unpriced,
                })).collect::<Vec<_>>(),
            },
            "supervision": {
                "done": n,
                "touched": touched,
                "touches": touches,
                "touches_per_done": ratio(touches),
                "touched_share_permille": (n > 0).then(|| touched * 1000 / n),
                "wait_on_person": spread(waits),
            },
        })
    }
}
