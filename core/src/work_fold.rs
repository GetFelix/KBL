//! The fold from a prefix of `work.events` to item state.
//!
//! Pure: no clock, no I/O, no model. [`State::apply`] takes one record and
//! either changes the state or returns why it did not. A rejected record is
//! kept in [`State::rejections`], which is the validation view: what a record
//! would have done had the tracker let it through. The live tracker and a
//! replay run the same code, so they agree by construction.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::artifact::ArtifactId;
use crate::names::{ItemId, Key};
use crate::work::{
    Actor, Assignee, CloseWhy, DoneWhen, Evidence, FORMAT, FORMAT_V1, FORMAT_V4, FORMAT_V5,
    FORMAT_V6, FORMAT_V8, FORMAT_V13, FORMAT_V14, FindingVerdict, MAX_ASK_OPTION_LEN,
    MAX_ASK_OPTIONS, MAX_CHECK_BYTES, MAX_FINDING_FILE, MAX_FINDING_TITLE, MAX_FINDINGS,
    MAX_PR_REF, MAX_REVERT_TEST, MAX_REVIEWER, MAX_REVIEWS_PER_ROOT, MAX_RULE_CHECKS,
    MAX_RULE_SCOPES, MAX_RULES_TEXT, MAX_SCENARIO, MAX_TITLE, MAX_USAGE_ENTRIES, MAX_VERDICT_WHY,
    MergeVerdict, NewPrerequisite, PrChecks, PrPhase, PrReview, PrSnapshot, PriceUnit,
    RefusalReason, ReleaseWhy, ReviewPass, ReviewSpec, TIERS, Target, Then, Unclear, WORK_EVENTS,
    WorkKind, WorkLogged, WorkRecord, valid_review_tier,
};

mod human_wait;
mod snapshots;
mod trust;
mod waiting_since;

pub use snapshots::{
    Anchor, FOLD_REV, Lineage, SNAPSHOT_EVERY, Saved, Snapshot, Snapshots, fold_to,
};

#[cfg(test)]
mod tests;

pub use human_wait::{HumanWaitStats, KindStats, QueueEntry, WaitKind};
pub use trust::{MIN_SAMPLE, TrustStats};
pub use waiting_since::WaitingSince;

const DEFAULT_MAX_ITEMS: u32 = 12;
const DEFAULT_MAX_DEPTH: u8 = 3;
const DEFAULT_COST_MICROS: u64 = 10_000_000;
const DEFAULT_REDOS: u8 = 3;
/// Claims one attempt may take, counting the first: a retry after a release,
/// a lapse or a give-up is the second.
const ATTEMPTS_ALLOWED: u32 = 2;
/// Attempts that may be open on one item at once: the current one and the one compared with it.
const OPEN_ATTEMPTS: usize = 2;
const DEFAULT_TIER: u8 = 2;
/// Longest rule name an `accepted` record carries as its triage-skipped reason.
const MAX_RULE_NAME: usize = 120;
/// What one attempt may write, per kind. A looping agent is refused at the cap
/// instead of filling the stream.
const MAX_DECISIONS: usize = 200;
const MAX_STEPS: usize = 500;
const MAX_EDITS: usize = 500;
const MAX_PROGRESS: u32 = 100;
const MAX_CONTEXT: u32 = 100;
const MAX_QUESTIONS: u32 = 20;
const MAX_ANSWERS: u32 = 10;
const MAX_LINKS: u32 = 100;
/// Distinct repos that may be reported unreadable at once.
const MAX_STALLS: usize = 64;
/// Children and proposals together.
const MAX_POSTS: u32 = 24;
/// Proposals an agent has waiting for a person, over all its attempts.
const MAX_PENDING_PER_AGENT: usize = 16;
/// Proposals waiting for a person that came from work under one root.
const MAX_PENDING_PER_ROOT: usize = 8;
/// Different triage answers one attempt may log. A repeat of the answer on record is not counted.
const MAX_TRIAGES: u32 = 4;
/// Distinct agents remembered as having worked an item, for the self-review rule.
const MAX_WORKERS: usize = 32;

/// A root's work budget. Agent records only ever lower it; a person's
/// `budget_set` is the one way up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub items_left: u32,
    pub max_depth: u8,
    pub cost_left_micros: u64,
    /// Redos one item may take. A limit, not a pool, so it never counts down.
    pub redos_per_item: u8,
}

impl Budget {
    fn for_new_root() -> Self {
        Budget {
            // The root takes one of the tree's slots.
            items_left: DEFAULT_MAX_ITEMS - 1,
            max_depth: DEFAULT_MAX_DEPTH,
            cost_left_micros: DEFAULT_COST_MICROS,
            redos_per_item: DEFAULT_REDOS,
        }
    }
}

/// What claims cost, summed over the reports that end or pause them (`done`,
/// `released`, `gave_up`, `question`, `prerequisite`). Derived from the
/// items, so it needs no record of its own.
///
/// From format 8 a report's cost is its usage priced with the prices in effect
/// when it was logged, or its floor if that is more. Below format 8 it is the
/// `cost_micros` the report carried.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spend {
    /// Everything charged, floors and costs reported before format 8 included.
    pub total_micros: u64,
    /// Per root, over the root and everything under it.
    pub by_root: BTreeMap<ItemId, u64>,
    /// Reviews only, per the reviewer tier each one asked for.
    pub by_review_tier: BTreeMap<String, u64>,
    /// Per `source:repo` of the item's root. Work under a root with no repo is
    /// in the total and its root only.
    pub by_repo: BTreeMap<String, u64>,
    /// Usage and its priced cost per model. The total is more than their sum
    /// by the floors and by costs reported before format 8.
    #[serde(default)]
    pub by_model: BTreeMap<String, ModelSpend>,
    /// Models with usage that has no price yet. That usage is charged the
    /// floor until a `price_set` prices it, with the unit that waits.
    #[serde(default)]
    pub unpriced: BTreeSet<ModelUnit>,
    /// Every price set, by model, unit and start, so a page can show what is in
    /// effect, what is scheduled and how it changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prices: Vec<PriceEntry>,
}

/// A model's unit, as the usage that waits for a price names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModelUnit {
    pub model: String,
    pub unit: PriceUnit,
}

/// One `price_set` as recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceEntry {
    pub model: String,
    pub unit: PriceUnit,
    pub price_micros: u64,
    /// When it starts, in milliseconds.
    pub effective_from_ms: u64,
    /// The vendor day an import copied it from; none for a price a person typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<String>,
}

/// The latest `price_set` for a model and unit at the time asked about:
/// `prices_in_effect(now)` is what prices usage now, and `prices_in_effect(u64::MAX)`
/// is the latest set, including one that has not started yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceInEffect {
    pub model: String,
    pub unit: PriceUnit,
    pub price_micros: u64,
    /// When this price took effect, in milliseconds.
    pub since_ms: u64,
}

/// One model's usage and what the priced part of it cost.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSpend {
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    /// AI credits, in thousandths.
    #[serde(default)]
    pub aic_milli: u64,
    /// The cost of the usage that has a price.
    #[serde(default)]
    pub cost_micros: u64,
    /// Some of the usage has no price yet.
    #[serde(default)]
    pub unpriced: bool,
}

impl ModelSpend {
    fn add_amount(&mut self, unit: PriceUnit, n: u64) {
        let slot = match unit {
            PriceUnit::Input => &mut self.input,
            PriceUnit::Output => &mut self.output,
            PriceUnit::CacheRead => &mut self.cache_read,
            PriceUnit::CacheWrite => &mut self.cache_write,
            PriceUnit::Aic => &mut self.aic_milli,
        };
        *slot = slot.saturating_add(n);
    }

    fn add(&mut self, o: &ModelSpend) {
        for (unit, n) in [
            (PriceUnit::Input, o.input),
            (PriceUnit::Output, o.output),
            (PriceUnit::CacheRead, o.cache_read),
            (PriceUnit::CacheWrite, o.cache_write),
            (PriceUnit::Aic, o.aic_milli),
        ] {
            self.add_amount(unit, n);
        }
        self.cost_micros = self.cost_micros.saturating_add(o.cost_micros);
        self.unpriced |= o.unpriced;
    }
}

/// A report with usage that had no price when it was logged. It was charged
/// the floor plus its priced part; a later price draws the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Unpriced {
    item: ItemId,
    root: ItemId,
    floor_micros: u64,
    /// The cost of the usage priced so far.
    priced_micros: u64,
    /// What the report was charged so far.
    charged_micros: u64,
    /// `(model, unit, amount)` still without a price.
    waiting: Vec<(String, PriceUnit, u64)>,
}

/// The latest `rules_set` for one scope: a repo, or a root in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesSet {
    pub target: Target,
    pub root: Option<ItemId>,
    pub text: Option<String>,
    pub checks: Option<Vec<String>>,
    /// The offset of the record, which is the version.
    pub version: u64,
}

/// The rules an item works under: the root's keys over the repo's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesInEffect {
    pub text: Option<String>,
    pub checks: Vec<String>,
    /// The newest of the records that contribute.
    pub version: u64,
    pub repo_version: Option<u64>,
    pub root_version: Option<u64>,
}

/// The board column an item sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// An agent's out-of-scope proposal, waiting for a person.
    Proposed,
    /// Nobody has accepted it yet.
    Triage,
    NeedsYou,
    Blocked,
    Ready,
    Held,
    /// An attempt finished and nobody has closed the item.
    InReview,
    /// Merged, or finished work that needs no merge.
    Done,
    /// Dismissed, closed upstream, cancelled with its root, or a rejected proposal.
    Closed,
}

/// How an item ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Closure {
    Merged,
    /// A non-fix item whose attempt finished.
    Completed,
    Dismissed,
    Upstream,
    /// An ancestor was closed.
    Cancelled,
    Rejected,
}

/// Why an item is in Needs you.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeedsYou {
    Question,
    OverBudget,
    /// A dependency was dismissed, rejected or cancelled. A person relinks or closes.
    DependencyDropped,
    /// The open attempt used its claims.
    TriesUsed,
    /// Every attempt was closed and none was redone.
    NoOpenAttempt,
    /// The upstream issue left its intake rule and nobody holds the item.
    LeftScope,
    /// A check on the pull request failed, which `Item::pr_status` names.
    CheckFailed,
    /// Triage could not settle what the item is, which `Item::ask` says.
    Unclear,
    /// A person moved it here with `flagged`.
    Flagged,
    /// A person recorded the merge and upstream shows the request not merged.
    MergeDisputed,
    /// A person took it: it would be Ready, and waits on them instead.
    Taken,
}

/// What the open question offers, kept so an answer can be checked and the
/// work that follows it can be built from the log alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ask {
    pub options: Vec<String>,
    pub default: Option<String>,
    /// The attempt that asked. Triage asks as attempt 0.
    pub attempt: u32,
    /// Set when the question is triage's: it could not settle the item.
    pub unclear: Option<Unclear>,
}

/// What an open pull request waits for, where the board words it specially.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrWaits {
    /// Checks pass and nobody has been asked to review.
    CiGreenNoReviewer,
    /// Approved and not merged: a person merges it.
    ApprovedNotMerged,
}

impl Item {
    /// Made by a `prerequisite` record. A deferred finding also targets a
    /// repo and waits for a person, but is not one; neither is an item an
    /// agent wrote a `prerequisite` context entry onto.
    pub fn is_prerequisite(&self) -> bool {
        self.finding.is_none()
            && self
                .context
                .keys()
                .any(|k| k.as_str() == "prerequisite" && !self.context_agents.contains_key(k))
    }

    /// The attempt number a claim must name: the open attempt for a retry after a
    /// release, lapse or give-up, a new one otherwise.
    pub fn next_attempt(&self) -> u32 {
        let retry = self.pending_fork.is_none() && !self.paths.is_empty();
        if retry {
            self.open_path().map_or(1, |p| p.attempt)
        } else {
            self.paths.last().map_or(1, |p| p.attempt + 1)
        }
    }

    /// The offset of the newest note on the item, addressed to an attempt or not.
    pub fn newest_note(&self) -> Option<u64> {
        let on_paths = self.paths.iter().flat_map(|p| p.human_inputs.iter());
        self.human_inputs.iter().chain(on_paths).max().copied()
    }

    /// The newest attempt that is not closed.
    pub fn open_path(&self) -> Option<&Path> {
        self.paths.iter().rev().find(|p| !p.closed)
    }
}

/// A snooze: when a person set it and until when My queue hides the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snooze {
    pub at_ms: u64,
    pub until_ms: u64,
    /// The reason the item waited when it was snoozed. A wait for another
    /// reason shows it again even when its start is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<WaitKind>,
}

/// A person's ask that someone review the pull request an item reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewAsk {
    pub reviewer: String,
    /// The request the ask was made on. A status for another request drops it.
    pub pr: String,
    /// What upstream showed when the ask was made. The ask is answered by a
    /// review that differs from it, not by one that repeats it.
    pub review: PrReview,
    pub at_ms: u64,
}

/// A person's record that the item's pull request merged, kept so the panel
/// and the API can tell it from a merge upstream reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeMark {
    pub by: Key,
    /// Offset of the `merge_marked` record.
    pub offset: u64,
}

/// A person's request that the pull request an item reports change before it merges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangesAsk {
    pub pr: String,
    pub at_ms: u64,
}

/// Longest a snooze may run, so a typo cannot hide an item for good.
pub const MAX_SNOOZE_MS: u64 = 30 * 86_400_000;

/// Who holds an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    pub by: Key,
    pub attempt: u32,
    pub deadline_ms: u64,
    /// When the holder last wrote a record on this attempt, in the tracker's clock.
    #[serde(default)]
    pub active_ms: u64,
}

/// What triage work proposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub tier: u8,
    pub why: String,
    pub duplicate_of: Option<ItemId>,
    #[serde(default)]
    pub unclear: Option<Unclear>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<ItemId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub split: Vec<String>,
    pub at: u64,
    /// Who wrote it, so a retry by the same writer can be told from a new holder's answer.
    /// Left out of the state's wire form; a replay of the log sets it again.
    #[serde(skip)]
    pub by: Option<Key>,
}

/// Where an attempt branched from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fork {
    pub from_attempt: u32,
    pub at: u64,
    pub note: Option<u64>,
}

/// One attempt. A redo is a new path that names the decision it branched at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Path {
    pub attempt: u32,
    /// Claims taken on this path, retries included.
    pub tries: u32,
    pub done: bool,
    /// Closed or abandoned by a person.
    pub closed: bool,
    pub forked_from: Option<Fork>,
    pub decisions: Vec<u64>,
    pub steps: Vec<u64>,
    pub edits: Vec<u64>,
    /// Offsets of the `human_input` records addressed to this attempt.
    pub human_inputs: Vec<u64>,
    #[serde(default)]
    pub progress: u32,
    /// Context entries the attempt wrote, on its item or the one it works for.
    #[serde(default)]
    pub context_writes: u32,
    /// Children and proposals the attempt created.
    #[serde(default)]
    pub posts: u32,
    /// Triage answers the attempt logged, as the holder of a triage item.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub triages: u32,
    /// Questions the attempt asked.
    #[serde(default)]
    pub questions: u32,
    /// Answers the attempt wrote, as the holder of a question item.
    #[serde(default)]
    pub answers: u32,
    /// Edges the attempt added.
    #[serde(default)]
    pub links: u32,
    /// A cap refusal is already on the log for this attempt.
    #[serde(default)]
    pub capped: bool,
    /// Findings the attempt recorded, when the item is a review.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub findings: u32,
    /// Offsets of the decisions whose revert check failed without the fix
    /// and passed with it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proven: Vec<u64>,
    /// A person's latest mark of the summary this attempt handed off: true if
    /// accurate. None until someone marks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_accurate: Option<bool>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Models with usage waiting for a price that a log may hold. A report that
/// would add one past this is refused until a person prices some.
const MAX_UNPRICED_MODELS: usize = 32;

/// Longest model name a report or price may carry.
const MAX_MODEL_LEN: usize = 100;

/// A real calendar day written `YYYY-MM-DD`, as an import writes the day it
/// copied prices. The tool and the dashboard refuse the same impossible days.
fn day_ok(d: &str) -> bool {
    let shaped = d.len() == 10
        && d.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    if !shaped {
        return false;
    }
    let (Ok(y), Ok(m), Ok(day)) = (
        d[..4].parse::<u32>(),
        d[5..7].parse::<u32>(),
        d[8..].parse::<u32>(),
    ) else {
        return false;
    };
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let last = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=last).contains(&day)
}

/// A model id as vendors write them: letters, digits and `.-_:/@`.
pub fn model_name_ok(m: &str) -> bool {
    !m.is_empty()
        && m.len() <= MAX_MODEL_LEN
        && m.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_:/@".contains(&b))
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A final review's recommendation, kept on the root it is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictNote {
    pub verdict: MergeVerdict,
    /// The final review that gave it.
    pub review: ItemId,
    /// The pull request it is about. Stale once the root reports another.
    pub pr: String,
    /// The offset of the `review_verdict` record, which holds the reason.
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    pub work: WorkKind,
    pub title: String,
    pub parent: Option<ItemId>,
    pub root: ItemId,
    pub depth: u8,
    pub children: Vec<ItemId>,
    pub proposed_by_agent: bool,
    /// The agent and root an agent's proposal came from, for the pending caps.
    /// Left out of the state's wire form; a replay of the log sets it again.
    #[serde(skip)]
    pub proposed_from: Option<(Key, ItemId)>,
    /// The upstream issue changed after the last triage answer.
    pub changed_upstream: bool,
    /// The upstream issue no longer matches the rule that admitted it.
    /// Cleared by `returned_to_scope` or a reopen.
    pub left_scope: bool,
    /// Upstream sync, not a person, took it out of scope, so sync may bring
    /// it back. Derived from the log, so it survives a lost ingest state.
    #[serde(default, skip_serializing_if = "is_false")]
    pub left_scope_by_sync: bool,
    /// A person moved it into Needs you. Cleared by `flagged: false` or a reopen.
    #[serde(default, skip_serializing_if = "is_false")]
    pub flagged: bool,
    /// A person hid it from My queue until a time. Applies only to the wait it
    /// was set on: a wait that begins later shows again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed: Option<Snooze>,
    /// When a person parked it, out of My queue until it waits for a new reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_at_ms: Option<u64>,
    /// The reason the item waited when it was parked, as for `Snooze::kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_kind: Option<WaitKind>,
    /// When the item began waiting in the inbox for its present reason. Items
    /// from a snapshot taken before this field have none until the reason changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_since: Option<WaitingSince>,
    /// A person asked someone to review the open pull request. Cleared once
    /// upstream shows a review, the request is no longer open, or it changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_ask: Option<ReviewAsk>,
    /// A person sent the open pull request back for changes. Cleared like `review_ask`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes_ask: Option<ChangesAsk>,
    /// A person recorded the merge. While the item is closed as merged this is
    /// who said so; if upstream then shows the request open or closed unmerged,
    /// the item reopens into Needs you with this still set. Cleared by a
    /// reopen or an upstream merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_marked: Option<MergeMark>,
    /// Who works it. A person assignee keeps it out of Ready. Kept across
    /// close and reopen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<Assignee>,
    pub accepted: bool,
    /// A person wrote the last `accepted` on a log at format 6 or above.
    /// An earlier acceptance does not count, so a format 5 state is unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub accepted_by_person: bool,
    /// The ingest rule that admitted this root with no triage item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage_skipped: Option<String>,
    pub tier: Option<u8>,
    pub proposal: Option<Proposal>,
    pub hold: Option<Hold>,
    /// From format 14: the hold the tracker ended as lapsed. Its agent may
    /// still ask or release as a wait until anyone claims the item again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lapsed: Option<Hold>,
    pub closed: Option<Closure>,
    /// The close was written by upstream sync (the tracker's role) and not by
    /// a person. Derived from the log, so it survives a lost ingest state.
    #[serde(default, skip_serializing_if = "is_false")]
    pub closed_by_sync: bool,
    /// Who wrote the last reopen: a person, or upstream sync as the tracker.
    /// Derived from the log, like `closed_by_sync`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reopened_by: Option<Actor>,
    pub redos: u8,
    pub over_budget: bool,
    pub open_question: Option<u64>,
    /// What the open question offers; set and cleared with `open_question`.
    #[serde(default)]
    pub ask: Option<Ask>,
    pub branch: Option<String>,
    pub pr: Option<String>,
    /// Upstream's view of `pr`, as the last `pr_status` said.
    #[serde(default)]
    pub pr_status: Option<PrSnapshot>,
    /// The offset of the `pr_status` that turned the checks red, while they
    /// stay red. A note newer than this was written after the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub red_since: Option<u64>,
    /// When the first claim on the item was logged, in the tracker's clock.
    #[serde(default)]
    pub first_claim_ms: Option<u64>,
    pub cost_micros: u64,
    /// Usage per model, from format 8.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub usage: BTreeMap<String, ModelSpend>,
    pub context: BTreeMap<Key, u64>,
    /// The agent that last wrote each entry an agent wrote. The fold accepts any replacement
    /// (old logs hold some); the tracker reads this to refuse one before it is logged.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context_agents: BTreeMap<Key, Key>,
    pub paths: Vec<Path>,
    /// Human inputs that name no attempt, and so steer every path.
    pub human_inputs: Vec<u64>,
    pub pending_fork: Option<Fork>,
    /// The repo the work lands in: derived for an imported root from its
    /// `owner/repo#number` id, named by an agent's prerequisite.
    #[serde(default)]
    pub target: Option<Target>,
    /// The release that carries this item, once a `shipped` record names it.
    #[serde(default)]
    pub release: Option<String>,
    /// Which review this is, when the item is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewSpec>,
    /// Offsets of the findings a review recorded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<u64>,
    /// The offset of the finding this item came from, when a review made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finding: Option<u64>,
    /// The latest final review's recommendation, on a root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_verdict: Option<VerdictNote>,
    /// Agents that claimed the item, so none of them reviews the work.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workers: Vec<Key>,
}

/// Why a record changed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reject {
    UnknownItem,
    /// The item, the child's key or the upstream id was already created.
    Duplicate,
    /// An agent created a child under an item it does not hold.
    NoHeldParent,
    OverBudget,
    TooDeep,
    NotHolder,
    NotReady,
    OpenChildren,
    Cycle,
    /// The actor may not write this kind of record.
    ActorNotAllowed,
    /// Evidence cites an offset that is not before the record.
    EvidenceFromTheFuture,
    MissingField,
    UnknownDecision,
    WrongAttempt,
    NoRedosLeft,
    NotHeld,
    UnknownInput,
    /// A path has used its claims.
    TooManyTries,
    /// Two attempts are open already.
    TooManyOpen,
    /// The item is closed.
    Closed,
    /// Only a proposal that was never accepted can be rejected.
    NotAProposal,
    /// Only a closed item can be reopened.
    NotClosed,
    /// Upstream sync records are about roots.
    NotARoot,
    /// The attempt wrote as many records of this kind as it may.
    LimitReached,
    /// A tier is 0 to 3.
    BadTier,
    /// An agent linked items of different trees.
    CrossRoot,
    /// An answer names a question that is not the open one.
    NotOpenQuestion,
    /// Options, a default or a chosen option are not what the question offers, or are over the bounds.
    BadOption,
    /// A pull request status names a request the item does not report.
    WrongPr,
    /// A text field is longer than a record keeps.
    TooLong,
    /// Upstream sync closed an item as merged without a merged status for its request.
    NotMerged,
    /// Another open item already reports this pull request.
    PrInUse,
    /// The agent worked on the pull request it was asked to review.
    SelfReview,
    /// The record is about a review and the item is not the right one.
    NotReview,
    /// The record's format is above the log's, below what its content needs,
    /// or above what this build reads; or a raise that does not go up.
    Format,
    /// From format 8: a report carries a cost of its own, more usage entries
    /// than a report keeps, a model twice, a model name that is empty, too
    /// long or not plain, or it would leave too many models waiting for a price.
    BadUsage,
    /// A `price_set` with a bad model name, or that takes effect before it was logged.
    BadPrice,
    /// A `flagged` record that would leave the item as it is.
    NoChange,
    /// A snooze that ends in the past or more than 30 days out.
    BadTime,
    /// An assignee the fold does not accept: an agent on a log below format 13.
    BadAssignee,
    /// A claim by an agent other than the item's agent assignee.
    NotAssignee,
    /// An edit of a proposal's advice that names no proposal, or one that was
    /// not read, or whose answer triage has replaced since.
    StaleProposal,
    /// Dependencies that repeat, name the item or its triage child, or name no item.
    BadDependency,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub offset: u64,
    pub why: Reject,
}

/// Every item, edge and budget as of `at`, the last offset applied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub at: Option<u64>,
    pub items: BTreeMap<ItemId, Item>,
    /// `(a, b)`: a waits on b.
    pub edges: BTreeSet<(ItemId, ItemId)>,
    /// Edges that wait for a release. Any edge not listed waits for a merge.
    #[serde(default)]
    pub release_edges: BTreeSet<(ItemId, ItemId)>,
    pub budgets: BTreeMap<ItemId, Budget>,
    /// The latest rules record per scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<RulesSet>,
    pub rejections: Vec<Rejection>,
    upstream: BTreeMap<String, ItemId>,
    /// `(source, repo)` to the HTTP status that stopped it being read.
    #[serde(default)]
    /// Keyed `source\0repo`: JSON object keys must be strings.
    source_stalls: BTreeMap<String, u16>,
    child_keys: BTreeSet<(Option<ItemId>, Option<Key>, Key)>,
    /// The log's format when a `format_raised` record raised it. None is the first format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    format: Option<u32>,
    /// Per model and unit, `(effective_from_ms, price_micros)` in effect order.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    prices: BTreeMap<String, BTreeMap<PriceUnit, Vec<(u64, u64)>>>,
    /// The `as_of` of a price, by model, unit and start. Two prices with the
    /// same start share one note: the later record's wins, as its price does.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    price_as_of: BTreeMap<String, BTreeMap<PriceUnit, BTreeMap<u64, String>>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unpriced: Vec<Unpriced>,
    /// Roots flagged over budget while a trial is running, to unflag on a refusal.
    #[serde(skip)]
    probing: Option<Vec<ItemId>>,
    /// The proposals waiting for a person, kept in step as the fold goes. A
    /// decoded state does not carry it, so only a folded one can answer.
    #[serde(skip)]
    waiting: Waiting,
    /// Waits on a person, open and finished. Derived by replay, so not in the wire form.
    #[serde(skip)]
    human_waits: human_wait::Waits,
    /// When each attempt's summary mark was written. Derived by replay, so not in the wire form.
    #[serde(skip)]
    mark_times: trust::MarkTimes,
    /// Which items an agent may hold, so a record finds its hold without a scan.
    #[serde(skip)]
    holders: Holders,
}

/// A superset of the items each agent holds, filled where a hold is set and
/// pruned when a lookup finds a hold gone. Derived from the items, so two
/// states are equal whether or not they carry it; a decoded state rebuilds
/// it on first use.
#[derive(Debug, Clone, Default)]
struct Holders {
    by: BTreeMap<Key, BTreeSet<ItemId>>,
    built: bool,
}

impl PartialEq for Holders {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Holders {}

impl Holders {
    fn note(&mut self, by: &Key, item: &ItemId) {
        self.by.entry(by.clone()).or_default().insert(item.clone());
    }
}

/// The root each proposal waiting for a person came from, by item, and how many
/// wait per root. Kept in step with `accepted`, `closed` and `proposed_from`
/// through `State::track`, so a post counts its root's proposals without
/// scanning every item. Derived from the items, so two states are equal
/// whether or not they hold it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Waiting {
    from: BTreeMap<ItemId, ItemId>,
    per_root: BTreeMap<ItemId, u32>,
}

impl PartialEq for Waiting {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Waiting {}

/// Whether a build that reads formats up to `reads` takes `rec` on a log at
/// format `log`. A raise answers with the new format. Everything else must
/// be stamped at or below the log's format, so no record lands that a build
/// still allowed on the log cannot read.
fn format_gate(reads: u32, log: u32, rec: &WorkLogged) -> Result<Option<u32>, Reject> {
    if rec.v > reads || rec.v < rec.record.format() {
        return Err(Reject::Format);
    }
    if let WorkRecord::FormatRaised { to } = rec.record {
        if rec.actor != Actor::Person {
            return Err(Reject::ActorNotAllowed);
        }
        // Stamped with the format it raises to, so a reader that cannot read
        // what follows stops at the raise itself.
        if to <= log || rec.v != to {
            return Err(Reject::Format);
        }
        return Ok(Some(to));
    }
    if rec.v > log {
        return Err(Reject::Format);
    }
    Ok(None)
}

/// Folds every record. Offsets must ascend.
pub fn fold<'a>(log: impl IntoIterator<Item = &'a (u64, WorkLogged)>) -> State {
    let mut state = State::default();
    for (offset, rec) in log {
        state.apply(*offset, rec);
    }
    state
}

impl State {
    /// Applies one record; a rejected one is noted in `rejections`.
    pub fn apply(&mut self, offset: u64, rec: &WorkLogged) {
        match self.step(offset, rec) {
            Ok(()) => self.touch_hold(rec),
            Err(why) => self.rejections.push(Rejection { offset, why }),
        }
        self.at = Some(offset);
    }

    /// Applies the record only if it is accepted; a refusal leaves the state as it
    /// was, rejections list included. The tracker uses this to validate without
    /// copying the state: it appends a record to the log only when this says yes.
    pub fn try_apply(&mut self, offset: u64, rec: &WorkLogged) -> Result<(), Reject> {
        self.probing = Some(Vec::new());
        let result = self.step(offset, rec);
        let flagged = self.probing.take().unwrap_or_default();
        match result {
            Ok(()) => {
                self.at = Some(offset);
                self.touch_hold(rec);
            }
            Err(_) => {
                for root in flagged {
                    if let Some(r) = self.items.get_mut(&root) {
                        r.over_budget = false;
                    }
                }
            }
        }
        result
    }

    /// Notes that the holder wrote, so a card can say how long an agent has been quiet.
    fn touch_hold(&mut self, rec: &WorkLogged) {
        if rec.actor != Actor::Agent {
            return;
        }
        if !self.holders.built {
            for it in self.items.values() {
                if let Some(h) = &it.hold {
                    self.holders.note(&h.by, &it.id);
                }
            }
            self.holders.built = true;
        }
        let Some(candidates) = self.holders.by.get_mut(&rec.by) else {
            return;
        };
        // Lowest id first, like a scan of the items. A candidate whose hold
        // is gone is dropped here; one is added again when a claim sets it.
        while let Some(id) = candidates.first().cloned() {
            match self
                .items
                .get_mut(&id)
                .and_then(|i| i.hold.as_mut())
                .filter(|h| h.by == rec.by)
            {
                Some(h) => {
                    h.active_ms = h.active_ms.max(rec.at_ms);
                    return;
                }
                None => {
                    candidates.remove(&id);
                }
            }
        }
        self.holders.by.remove(&rec.by);
    }

    /// The open item `agent` holds, with the attempt, if any.
    pub fn held_by(&self, agent: &str) -> Option<(ItemId, u32)> {
        self.items.values().find_map(|i| {
            let h = i.hold.as_ref()?;
            (i.closed.is_none() && h.by.as_str() == agent).then(|| (i.id.clone(), h.attempt))
        })
    }

    /// Whether an open item's link waits for a release of `item`. Only then
    /// does ingest look for the release that carries it.
    pub fn awaits_release(&self, item: &ItemId) -> bool {
        self.release_edges
            .iter()
            .any(|(from, to)| to == item && !self.is_closed(from))
    }

    /// Every imported root with its source and upstream id.
    pub fn imported(&self) -> impl Iterator<Item = (&str, &str, &ItemId)> {
        self.upstream.iter().filter_map(|(up, item)| {
            let (source, id) = up.split_once('\n')?;
            Some((source, id, item))
        })
    }

    /// Open prerequisites a person accepted from format 6 that have no
    /// upstream issue yet: what ingest may open an issue for. Each comes with
    /// the upstream id of a root waiting on it, if one is imported.
    pub fn unopened_prerequisites(&self) -> Vec<(&Item, Option<&str>)> {
        self.items
            .values()
            .filter(|it| {
                it.parent.is_none()
                    && it.target.is_some()
                    && it.closed.is_none()
                    && it.accepted_by_person
                    && it.is_prerequisite()
                    && !self.upstream.values().any(|i| *i == it.id)
            })
            .map(|it| {
                let waiter = self.edges.iter().find_map(|(w, p)| {
                    let root = &self.items.get(w)?.root;
                    let up = self.upstream.iter().find(|(_, i)| *i == root)?.0;
                    (*p == it.id).then(|| up.split_once('\n').map_or(up.as_str(), |(_, id)| id))
                });
                (it, waiter)
            })
            .collect()
    }

    /// Spend per root, reviewer tier and repo. Items that cost nothing are left out.
    pub fn spend(&self) -> Spend {
        let mut s = Spend::default();
        for (model, units) in &self.prices {
            for (unit, list) in units {
                for (from, price_micros) in list {
                    s.prices.push(PriceEntry {
                        model: model.clone(),
                        unit: *unit,
                        price_micros: *price_micros,
                        effective_from_ms: *from,
                        as_of: self
                            .price_as_of
                            .get(model)
                            .and_then(|u| u.get(unit))
                            .and_then(|d| d.get(from))
                            .cloned(),
                    });
                }
            }
        }
        for u in &self.unpriced {
            s.unpriced
                .extend(u.waiting.iter().map(|(model, unit, _)| ModelUnit {
                    model: model.clone(),
                    unit: *unit,
                }));
        }
        for it in self
            .items
            .values()
            .filter(|it| it.cost_micros > 0 || !it.usage.is_empty())
        {
            for (model, m) in &it.usage {
                s.by_model.entry(model.clone()).or_default().add(m);
            }
            let cost = it.cost_micros;
            s.total_micros = s.total_micros.saturating_add(cost);
            let by_root = s.by_root.entry(it.root.clone()).or_default();
            *by_root = by_root.saturating_add(cost);
            if let Some(r) = &it.review {
                let tier = s.by_review_tier.entry(r.tier.clone()).or_default();
                *tier = tier.saturating_add(cost);
            }
            // The root's repo, as for rules: a child lands where its root does.
            let target = self
                .items
                .get(&it.root)
                .and_then(|r| r.target.as_ref())
                .or(it.target.as_ref());
            if let Some(t) = target {
                let repo = s
                    .by_repo
                    .entry(format!("{}:{}", t.source, t.repo))
                    .or_default();
                *repo = repo.saturating_add(cost);
            }
        }
        s
    }

    /// The rules in effect for `id`: its repo's, with the root's own keys on top.
    pub fn rules_for(&self, id: &ItemId) -> Option<RulesInEffect> {
        let root = self.items.get(&self.items.get(id)?.root)?;
        let repo = root.target.as_ref().and_then(|t| {
            self.rules
                .iter()
                .find(|r| r.root.is_none() && &r.target == t)
        });
        let own = self
            .rules
            .iter()
            .find(|r| r.root.as_ref() == Some(&root.id));
        let layers = [own, repo];
        let text = layers.iter().flatten().find_map(|r| r.text.clone());
        let checks = layers
            .iter()
            .flatten()
            .find_map(|r| r.checks.clone())
            .unwrap_or_default();
        let version = layers.iter().flatten().map(|r| r.version).max()?;
        Some(RulesInEffect {
            text,
            checks,
            version,
            repo_version: repo.map(|r| r.version),
            root_version: own.map(|r| r.version),
        })
    }

    /// The source each imported root came from, as its `imported` record named it.
    /// Built in one pass, so a caller asking for every item does not scan per item.
    pub fn imported_sources(&self) -> BTreeMap<&ItemId, &str> {
        let mut sources = BTreeMap::new();
        for (key, root) in &self.upstream {
            if let Some((source, _)) = key.split_once('\n') {
                sources.entry(root).or_insert(source);
            }
        }
        sources
    }

    /// How many items sit in In review: attempts agents finished that nobody
    /// has closed.
    pub fn in_review_count(&self) -> usize {
        self.items
            .keys()
            .filter(|id| self.stage(id) == Some(Stage::InReview))
            .count()
    }

    /// What the review limit counts: items in In review, plus fixes agents hold
    /// that are headed there. A fix a review asked for (`finding`) is left out:
    /// it closes without a PR of its own and is never paused.
    pub fn review_load(&self) -> usize {
        let held = self
            .items
            .iter()
            .filter(|(id, it)| {
                it.work == WorkKind::Fix
                    && it.finding.is_none()
                    && self.stage(id) == Some(Stage::Held)
            })
            .count();
        self.in_review_count() + held
    }

    pub fn stage(&self, id: &ItemId) -> Option<Stage> {
        let it = self.items.get(id)?;
        Some(match it.closed {
            Some(Closure::Merged | Closure::Completed) => Stage::Done,
            Some(_) => Stage::Closed,
            None if self.needs_you(id).is_some() => Stage::NeedsYou,
            None if it.hold.is_some() => Stage::Held,
            None if !it.accepted => {
                if it.proposed_by_agent {
                    Stage::Proposed
                } else {
                    Stage::Triage
                }
            }
            None => self.accepted_stage(id, it),
        })
    }

    /// The stage of an open, accepted item that nobody holds and that does
    /// not need a person.
    fn accepted_stage(&self, id: &ItemId, it: &Item) -> Stage {
        if it.pending_fork.is_none() && it.open_path().is_some_and(|p| p.done) {
            Stage::InReview
        } else if self.links_from(id).any(|b| self.waits_on(id, b)) || self.resuming(id) {
            Stage::Blocked
        } else {
            Stage::Ready
        }
    }

    /// Why an open item waits on a person, if it does.
    pub fn needs_you(&self, id: &ItemId) -> Option<NeedsYou> {
        let it = self.items.get(id)?;
        if it.closed.is_some() {
            return None;
        }
        if it.open_question.is_some() {
            if it.ask.as_ref().is_some_and(|a| a.unclear.is_some()) {
                return Some(NeedsYou::Unclear);
            }
            return Some(NeedsYou::Question);
        }
        if it.over_budget {
            return Some(NeedsYou::OverBudget);
        }
        if it.merge_marked.is_some() {
            return Some(NeedsYou::MergeDisputed);
        }
        // A holder may finish or release first; only then does the item stop.
        if it.left_scope && it.hold.is_none() {
            return Some(NeedsYou::LeftScope);
        }
        // From format 4 a redo is what the person asked for, so it is handed
        // out instead of waiting for green, until its attempt is done. Its claim
        // may be released and retried. Each failure needs a person to read it:
        // the item waits until it has a note newer than the failure, so a
        // redo that goes red again is freed by a note, not a new redo.
        let redo = it.pending_fork.is_some()
            || it
                .open_path()
                .is_some_and(|p| !p.done && p.forked_from.is_some());
        let redo_runs = self.format() >= FORMAT_V4
            && redo
            && it.newest_note().is_some_and(|n| Some(n) > it.red_since);
        if it.hold.is_none()
            && !redo_runs
            && it
                .pr_status
                .as_ref()
                .is_some_and(|s| s.phase == PrPhase::Open && s.checks == PrChecks::Red)
        {
            return Some(NeedsYou::CheckFailed);
        }
        let dropped = self.links_from(id).any(|b| {
            self.items.get(b).is_some_and(|d| {
                matches!(
                    d.closed,
                    Some(
                        Closure::Dismissed
                            | Closure::Upstream
                            | Closure::Cancelled
                            | Closure::Rejected
                    )
                )
            })
        });
        if dropped {
            return Some(NeedsYou::DependencyDropped);
        }
        // A closed attempt is never claimable: only a redo reopens the item.
        if it.pending_fork.is_none() && it.hold.is_none() && !it.paths.is_empty() {
            match it.open_path() {
                None => return Some(NeedsYou::NoOpenAttempt),
                Some(p) if !p.done && p.tries >= ATTEMPTS_ALLOWED => {
                    return Some(NeedsYou::TriesUsed);
                }
                Some(_) => {}
            }
        }
        // Last, so an item that also waits for another reason shows that one.
        if it.flagged && it.hold.is_none() {
            return Some(NeedsYou::Flagged);
        }
        // A person works it, so no agent may claim it: where it would be Ready
        // it waits on them. A holder finishes or releases first.
        if matches!(it.assignee, Some(Assignee::Person(_)))
            && it.hold.is_none()
            && it.accepted
            && self.accepted_stage(id, it) == Stage::Ready
        {
            return Some(NeedsYou::Taken);
        }
        None
    }

    /// The wording an open pull request gets on the board, if it has one.
    /// A red check outranks both: that item is in Needs you, or Ready with a redo waiting.
    pub fn pr_waits(&self, id: &ItemId) -> Option<PrWaits> {
        let it = self.items.get(id)?;
        let s = it.pr_status.as_ref().filter(|s| s.phase == PrPhase::Open)?;
        match (s.checks, s.review) {
            (PrChecks::Red, _) => None,
            (_, PrReview::Approved) => Some(PrWaits::ApprovedNotMerged),
            (PrChecks::Green, PrReview::Unreviewed) => Some(PrWaits::CiGreenNoReviewer),
            _ => None,
        }
    }

    /// Whether the root's final review recommends a merge of the request it
    /// still reports and no fix a review asked for is open. A person merges.
    pub fn merge_ready(&self, root: &ItemId) -> bool {
        self.items.get(root).is_some_and(|r| {
            r.closed.is_none()
                && r.review_verdict.as_ref().is_some_and(|v| {
                    v.verdict == MergeVerdict::Merge && r.pr.as_deref() == Some(v.pr.as_str())
                })
                && !self.open_fixes(root)
        })
    }

    /// Repositories a source cannot read, with the HTTP status it got.
    pub fn stalled_sources(&self) -> impl Iterator<Item = (&str, &str, u16)> {
        self.source_stalls.iter().filter_map(|(k, status)| {
            let (source, repo) = k.split_once('\0')?;
            Some((source, repo, *status))
        })
    }

    /// Children of `id` that are not done.
    pub fn open_children(&self, id: &ItemId) -> Vec<&ItemId> {
        self.items.get(id).map_or_else(Vec::new, |it| {
            it.children.iter().filter(|c| !self.is_closed(c)).collect()
        })
    }

    /// Human input offsets that steer `attempt`: those aimed at the whole
    /// item, the ones inherited from the attempts it branched from up to the
    /// branch point, and its own.
    pub fn human_inputs_on_path(&self, id: &ItemId, attempt: u32) -> Vec<u64> {
        let Some(it) = self.items.get(id) else {
            return Vec::new();
        };
        let mut chain = Vec::new();
        let mut cur = Some(attempt);
        while let Some(a) = cur {
            let Some(p) = it.paths.iter().find(|p| p.attempt == a) else {
                break;
            };
            chain.push(p);
            cur = p.forked_from.as_ref().map(|f| f.from_attempt);
        }
        let mut out = it.human_inputs.clone();
        for (i, p) in chain.iter().enumerate().rev() {
            // A path carries its parent's inputs only up to where it branched.
            let limit = if i == 0 {
                u64::MAX
            } else {
                chain[i - 1].forked_from.as_ref().map_or(u64::MAX, |f| f.at)
            };
            out.extend(p.human_inputs.iter().copied().filter(|o| *o <= limit));
        }
        // The steering note may have been written after the fork decision.
        out.extend(chain.iter().filter_map(|p| p.forked_from.as_ref()?.note));
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Whether the edge "`from` waits on `to`" still holds `from`. A dropped
    /// `to` frees it here and sends `from` to Needs you instead.
    fn waits_on(&self, from: &ItemId, to: &ItemId) -> bool {
        let Some(dep) = self.items.get(to) else {
            return true;
        };
        match dep.closed {
            None => true,
            Some(Closure::Merged | Closure::Completed) => {
                dep.release.is_none() && self.release_edges.contains(&(from.clone(), to.clone()))
            }
            Some(_) => false,
        }
    }

    /// What `id` waits on, open or not. A range scan, not a walk of every edge.
    fn links_from<'a>(&'a self, id: &ItemId) -> impl Iterator<Item = &'a ItemId> + use<'a> {
        let owner = id.clone();
        // "-" sorts before every other valid name, so it bounds the range from below.
        let lowest = ItemId::new("-").expect("a valid name");
        self.edges
            .range((owner.clone(), lowest)..)
            .take_while(move |(a, _)| *a == owner)
            .map(|(_, b)| b)
    }

    /// Whether a link says `from` waits on `to`, open or not.
    pub fn is_linked(&self, from: &ItemId, to: &ItemId) -> bool {
        self.edges.contains(&(from.clone(), to.clone()))
    }

    /// The items that hold `id` back now, in id order. A link holds whatever
    /// tree, repo or source the other end is in.
    pub fn blockers(&self, id: &ItemId) -> Vec<&ItemId> {
        self.links_from(id)
            .filter(|b| self.waits_on(id, b))
            .collect()
    }

    /// The repo an item's work lands in: its own target (a prerequisite's),
    /// else its root's. A GitHub or GitLab target names a repo; an ADO target is an
    /// `org/project` that drives rules and spend but is not shown as a repo.
    pub fn repo_of(&self, id: &ItemId) -> Option<&Target> {
        let it = self.items.get(id)?;
        it.target
            .as_ref()
            .or_else(|| self.items.get(&it.root)?.target.as_ref())
            .filter(|t| matches!(t.source.as_str(), "github" | "gitlab"))
    }

    fn is_closed(&self, id: &ItemId) -> bool {
        self.items.get(id).is_some_and(|i| i.closed.is_some())
    }

    /// Whether work that carries on from an answered question is still open
    /// under `id`. The asking item stays out of Ready meanwhile, so nobody
    /// claims it alongside the agent that took the resume.
    fn resuming(&self, id: &ItemId) -> bool {
        self.items.get(id).is_some_and(|it| {
            it.children.iter().any(|c| {
                self.items
                    .get(c)
                    .is_some_and(|c| c.work == WorkKind::Resume && c.closed.is_none())
            })
        })
    }

    /// Marks the root over budget so the stage does not depend on a later `refused`.
    fn flag_over_budget(&mut self, root: &ItemId) {
        if let Some(r) = self.items.get_mut(root) {
            if !r.over_budget
                && let Some(flagged) = &mut self.probing
            {
                flagged.push(root.clone());
            }
            r.over_budget = true;
        }
    }

    fn item(&self, id: &ItemId) -> Result<&Item, Reject> {
        self.items.get(id).ok_or(Reject::UnknownItem)
    }

    fn item_mut(&mut self, id: &ItemId) -> Result<&mut Item, Reject> {
        self.items.get_mut(id).ok_or(Reject::UnknownItem)
    }

    /// The item, where an agent must hold it on `attempt`; others may act freely.
    fn acting_on(&self, id: &ItemId, attempt: u32, rec: &WorkLogged) -> Result<&Item, Reject> {
        let it = self.item(id)?;
        if rec.actor == Actor::Agent {
            match &it.hold {
                Some(h) if h.by == rec.by && h.attempt == attempt => {}
                _ => return Err(Reject::NotHolder),
            }
        } else if it.hold.as_ref().is_some_and(|h| h.attempt != attempt) {
            // A person or the tracker must name the attempt that is running.
            return Err(Reject::WrongAttempt);
        }
        Ok(it)
    }

    /// Whether the writer may make the record if it cites evidence: the holder
    /// for a decision or prerequisite, someone who may answer for an answer.
    /// Lets the tracker check what evidence points at only for a writer the
    /// fold would let act.
    pub fn may_cite(&self, rec: &WorkLogged) -> Result<(), Reject> {
        match &rec.record {
            WorkRecord::Decision { item, attempt, .. } => self.agent_holder(item, *attempt, rec),
            WorkRecord::Prerequisite {
                item,
                attempt,
                triage,
                ..
            } => self
                .prerequisite_holder(item, *attempt, triage.as_ref(), rec)
                .map(|_| ()),
            WorkRecord::Answer { item, .. } => self.may_answer(item, rec).map(|_| ()),
            _ => Ok(()),
        }
    }

    /// The item whose holder may write a prerequisite for `item`: `item`
    /// itself, or with `triage` the triage item under it. Closing `item`
    /// ends that claim, so a held triage item means `item` is open.
    fn prerequisite_holder<'a>(
        &self,
        item: &'a ItemId,
        attempt: u32,
        triage: Option<&'a ItemId>,
        rec: &WorkLogged,
    ) -> Result<&'a ItemId, Reject> {
        let holder = triage.unwrap_or(item);
        self.agent_holder(holder, attempt, rec)?;
        if let Some(t) = triage {
            let t = self.item(t)?;
            if t.work != WorkKind::Triage || t.parent.as_ref() != Some(item) {
                return Err(Reject::NotHolder);
            }
        }
        Ok(holder)
    }

    /// Sets who works an open, accepted item, for `taken` and `reassigned`.
    fn assign(
        &mut self,
        id: &ItemId,
        to: Option<Assignee>,
        rec: &WorkLogged,
    ) -> Result<(), Reject> {
        if rec.actor != Actor::Person {
            return Err(Reject::ActorNotAllowed);
        }
        let it = self.item_mut(id)?;
        if it.closed.is_some() {
            return Err(Reject::Closed);
        }
        if !it.accepted {
            return Err(Reject::NotReady);
        }
        if it.assignee == to {
            return Err(Reject::NoChange);
        }
        it.assignee = to;
        Ok(())
    }

    fn agent_holder(&self, id: &ItemId, attempt: u32, rec: &WorkLogged) -> Result<(), Reject> {
        if rec.actor != Actor::Agent {
            return Err(Reject::ActorNotAllowed);
        }
        self.acting_on(id, attempt, rec).map(|_| ())
    }

    /// From format 14, whether `rec` is from the agent whose hold on `attempt`
    /// lapsed, with nobody holding the item since. Such an agent may still ask
    /// or release as a wait, once: either ends the late window.
    fn late_writer(&self, id: &ItemId, attempt: u32, rec: &WorkLogged) -> Result<bool, Reject> {
        let it = self.item(id)?;
        Ok(self.format() >= FORMAT_V14
            && rec.actor == Actor::Agent
            && it.hold.is_none()
            && it.closed.is_none()
            && it.open_question.is_none()
            && it.pending_fork.is_none()
            && it
                .lapsed
                .as_ref()
                .is_some_and(|h| h.by == rec.by && h.attempt == attempt)
            && it
                .paths
                .last()
                .is_some_and(|p| p.attempt == attempt && !p.done && !p.closed))
    }

    /// Ends a hold (or a late window) as a wait: charged like a release, and
    /// the claim it ended is given back. Only failures use up a try.
    fn park_wait(
        &mut self,
        id: &ItemId,
        attempt: u32,
        rec: &WorkLogged,
        held: bool,
    ) -> Result<(), Reject> {
        if held {
            self.acting_on(id, attempt, rec)?;
        }
        let root = self.item(id)?.root.clone();
        self.charge(id, &root, rec);
        let it = self.item_mut(id)?;
        it.hold = None;
        it.lapsed = None;
        if let Some(p) = it.paths.iter_mut().find(|p| p.attempt == attempt) {
            p.tries = p.tries.saturating_sub(1);
        }
        Ok(())
    }

    fn check_evidence(offset: u64, evidence: &[Evidence]) -> Result<(), Reject> {
        // Only work.events offsets can be compared with this record's own.
        if evidence
            .iter()
            .any(|e| e.stream == WORK_EVENTS && e.offset >= offset)
        {
            return Err(Reject::EvidenceFromTheFuture);
        }
        Ok(())
    }

    fn new_item(&mut self, item: Item) {
        if let Some(p) = &item.parent
            && let Some(parent) = self.items.get_mut(p)
        {
            parent.children.push(item.id.clone());
        }
        let id = item.id.clone();
        self.items.insert(id.clone(), item);
        self.track(&id);
    }

    /// Brings the waiting-proposal count in line with `id` after anything
    /// that can change whether it is a proposal waiting for a person.
    fn track(&mut self, id: &ItemId) {
        let now = self
            .items
            .get(id)
            .filter(|i| !i.accepted && i.closed.is_none())
            .and_then(|i| i.proposed_from.as_ref())
            .map(|f| f.1.clone());
        if self.waiting.from.get(id) == now.as_ref() {
            return;
        }
        if let Some(old) = self.waiting.from.remove(id)
            && let Some(n) = self.waiting.per_root.get_mut(&old)
        {
            *n -= 1;
            if *n == 0 {
                self.waiting.per_root.remove(&old);
            }
        }
        if let Some(root) = now {
            *self.waiting.per_root.entry(root.clone()).or_default() += 1;
            self.waiting.from.insert(id.clone(), root);
        }
    }

    fn blank(id: &ItemId, work: WorkKind, title: &str) -> Item {
        Item {
            id: id.clone(),
            work,
            title: title.to_string(),
            parent: None,
            root: id.clone(),
            depth: 0,
            children: Vec::new(),
            proposed_by_agent: false,
            proposed_from: None,
            changed_upstream: false,
            left_scope: false,
            flagged: false,
            snoozed: None,
            parked_at_ms: None,
            parked_kind: None,
            waiting_since: None,
            review_ask: None,
            changes_ask: None,
            merge_marked: None,
            assignee: None,
            left_scope_by_sync: false,
            accepted: false,
            accepted_by_person: false,
            triage_skipped: None,
            tier: None,
            proposal: None,
            hold: None,
            closed: None,
            closed_by_sync: false,
            reopened_by: None,
            redos: 0,
            over_budget: false,
            open_question: None,
            lapsed: None,
            ask: None,
            branch: None,
            pr: None,
            pr_status: None,
            red_since: None,
            first_claim_ms: None,
            cost_micros: 0,
            usage: BTreeMap::new(),
            context: BTreeMap::new(),
            context_agents: BTreeMap::new(),
            paths: Vec::new(),
            human_inputs: Vec::new(),
            pending_fork: None,
            target: None,
            release: None,
            review: None,
            findings: Vec::new(),
            finding: None,
            review_verdict: None,
            workers: Vec::new(),
        }
    }

    fn draw_cost(&mut self, root: &ItemId, micros: u64) {
        if let Some(b) = self.budgets.get_mut(root) {
            b.cost_left_micros = b.cost_left_micros.saturating_sub(micros);
        }
    }

    /// Every price in effect at `at_ms`, by model and unit.
    pub fn prices_in_effect(&self, at_ms: u64) -> Vec<PriceInEffect> {
        let mut out = Vec::new();
        for (model, units) in &self.prices {
            for (unit, list) in units {
                if let Some((since_ms, price_micros)) =
                    list.iter().rev().find(|(from, _)| *from <= at_ms)
                {
                    out.push(PriceInEffect {
                        model: model.clone(),
                        unit: *unit,
                        price_micros: *price_micros,
                        since_ms: *since_ms,
                    });
                }
            }
        }
        out
    }

    /// The price of one unit of `model` for usage logged at `at_ms`: the latest
    /// in effect then.
    fn price(&self, model: &str, unit: PriceUnit, at_ms: u64) -> Option<u64> {
        let list = self.prices.get(model)?.get(&unit)?;
        list.iter()
            .rev()
            .find(|(from, _)| *from <= at_ms)
            .map(|(_, p)| *p)
    }

    /// Charges `item` and draws `root`'s budget for what the record reports.
    /// Usage with no price is charged nothing until a price comes, but the
    /// report is charged at least its floor meanwhile.
    fn charge(&mut self, item: &ItemId, root: &ItemId, rec: &WorkLogged) {
        let Some(c) = rec.record.charge() else {
            return;
        };
        let mut usage: BTreeMap<String, ModelSpend> = BTreeMap::new();
        let mut priced = 0u64;
        let mut waiting = Vec::new();
        for u in c
            .usage
            .iter()
            .filter(|u| u.amounts().iter().any(|(_, n)| *n > 0))
        {
            let m = usage.entry(u.model().to_owned()).or_default();
            for (unit, n) in u.amounts().into_iter().filter(|(_, n)| *n > 0) {
                m.add_amount(unit, n);
                match self.price(u.model(), unit, rec.at_ms) {
                    Some(p) => {
                        let cost = unit.cost_micros(n, p);
                        m.cost_micros = m.cost_micros.saturating_add(cost);
                        priced = priced.saturating_add(cost);
                    }
                    None => {
                        m.unpriced = true;
                        waiting.push((u.model().to_owned(), unit, n));
                    }
                }
            }
        }
        let charged = c.floor_micros.max(priced);
        let total = c.cost_micros.saturating_add(charged);
        if let Some(it) = self.items.get_mut(item) {
            it.cost_micros = it.cost_micros.saturating_add(total);
            for (model, m) in &usage {
                it.usage.entry(model.clone()).or_default().add(m);
            }
        }
        self.draw_cost(root, total);
        if !waiting.is_empty() {
            self.unpriced.push(Unpriced {
                item: item.clone(),
                root: root.clone(),
                floor_micros: c.floor_micros,
                priced_micros: priced,
                charged_micros: charged,
                waiting,
            });
        }
    }

    /// Prices usage that was waiting for `model`'s `unit`, and draws what it
    /// costs beyond what its report was already charged.
    fn reprice(&mut self, model: &str, unit: PriceUnit, price_micros: u64) {
        let mut draws = Vec::new();
        for u in &mut self.unpriced {
            let before = u.waiting.len();
            let mut gained = 0u64;
            u.waiting.retain(|(m, un, n)| {
                let hit = m == model && *un == unit;
                if hit {
                    gained = gained.saturating_add(unit.cost_micros(*n, price_micros));
                }
                !hit
            });
            if u.waiting.len() == before {
                continue;
            }
            u.priced_micros = u.priced_micros.saturating_add(gained);
            let now = u.floor_micros.max(u.priced_micros);
            let extra = now.saturating_sub(u.charged_micros);
            u.charged_micros = now;
            draws.push((u.item.clone(), u.root.clone(), gained, extra));
        }
        self.unpriced.retain(|u| !u.waiting.is_empty());
        for (item, root, gained, extra) in draws {
            let still = self
                .unpriced
                .iter()
                .any(|u| u.item == item && u.waiting.iter().any(|(m, _, _)| m == model));
            if let Some(it) = self.items.get_mut(&item) {
                it.cost_micros = it.cost_micros.saturating_add(extra);
                if let Some(m) = it.usage.get_mut(model) {
                    m.cost_micros = m.cost_micros.saturating_add(gained);
                    m.unpriced = still;
                }
            }
            self.draw_cost(&root, extra);
        }
    }

    /// Models with usage that has no price yet.
    pub fn unpriced_models(&self) -> BTreeSet<&str> {
        self.unpriced
            .iter()
            .flat_map(|u| u.waiting.iter().map(|(m, _, _)| m.as_str()))
            .collect()
    }

    /// Checks the cost fields of a report before anything changes.
    fn check_charge(&self, rec: &WorkLogged) -> Result<(), Reject> {
        let Some(c) = rec.record.charge() else {
            return Ok(());
        };
        if self.format() < FORMAT_V8 {
            return Ok(());
        }
        if c.cost_micros > 0 || c.usage.len() > MAX_USAGE_ENTRIES {
            return Err(Reject::BadUsage);
        }
        let mut seen = BTreeSet::new();
        for u in c.usage {
            if !model_name_ok(u.model()) || !seen.insert(u.model()) {
                return Err(Reject::BadUsage);
            }
        }
        // Each made-up model would wait for a price forever, so their number is bounded.
        let mut unpriced = self.unpriced_models();
        for u in c.usage {
            let waits = u
                .amounts()
                .iter()
                .any(|(unit, n)| *n > 0 && self.price(u.model(), *unit, rec.at_ms).is_none());
            if waits {
                unpriced.insert(u.model());
            }
        }
        if unpriced.len() > MAX_UNPRICED_MODELS {
            return Err(Reject::BadUsage);
        }
        Ok(())
    }

    /// The highest format a record on this log may have. A `format_raised`
    /// record raises it.
    pub fn format(&self) -> u32 {
        self.format.unwrap_or(FORMAT_V1)
    }

    fn step(&mut self, offset: u64, rec: &WorkLogged) -> Result<(), Reject> {
        self.step_record(offset, rec)?;
        self.track_human_wait(rec);
        self.track_waiting_since(offset, rec);
        Ok(())
    }

    fn step_record(&mut self, offset: u64, rec: &WorkLogged) -> Result<(), Reject> {
        if let Some(raised) = format_gate(FORMAT, self.format(), rec)? {
            self.format = Some(raised);
            return Ok(());
        }
        self.check_charge(rec)?;
        match &rec.record {
            WorkRecord::Imported {
                item,
                source,
                upstream_id,
                title,
            } => {
                if rec.actor == Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                let up = format!("{source}\n{upstream_id}");
                if self.upstream.contains_key(&up) {
                    return Err(Reject::Duplicate);
                }
                if self.items.contains_key(item) {
                    if self.format() < FORMAT_V6 {
                        return Err(Reject::Duplicate);
                    }
                    return self.attach_upstream(item, source, upstream_id, up);
                }
                self.upstream.insert(up, item.clone());
                self.budgets.insert(item.clone(), Budget::for_new_root());
                let mut root = Self::blank(item, WorkKind::Fix, title);
                root.target = upstream_id.split_once('#').and_then(|(repo, _)| {
                    repo.contains('/').then(|| Target {
                        source: source.clone(),
                        repo: repo.to_owned(),
                    })
                });
                self.new_item(root);
            }
            WorkRecord::Proposed {
                item,
                parent,
                key,
                work,
                title,
                review,
            } => self.propose(
                rec,
                item,
                parent.as_ref(),
                key,
                *work,
                title,
                review.as_ref(),
            )?,
            WorkRecord::Triaged {
                item,
                tier,
                why,
                duplicate_of,
                unclear,
                depends_on,
                split,
            } => {
                if *tier >= TIERS {
                    return Err(Reject::BadTier);
                }
                let held = self.may_triage(item, rec)?;
                if let Some((triage, attempt)) = held {
                    let same = self.item(item)?.proposal.as_ref().is_some_and(|p| {
                        p.tier == *tier
                            && p.why == *why
                            && p.duplicate_of == *duplicate_of
                            && p.unclear == *unclear
                            && p.depends_on == *depends_on
                            && p.split == *split
                    });
                    if !same && let Ok(path) = self.path_mut(&triage, attempt) {
                        path.triages = path.triages.saturating_add(1);
                    }
                }
                self.item_mut(item)?.proposal = Some(Proposal {
                    tier: *tier,
                    why: why.clone(),
                    duplicate_of: duplicate_of.clone(),
                    unclear: *unclear,
                    depends_on: depends_on.clone(),
                    split: split.clone(),
                    at: offset,
                    by: Some(rec.by.clone()),
                });
                let it = self.item_mut(item)?;
                it.changed_upstream = false;
                // An unclear item reaches a person as a question, not as Ready work.
                if unclear.is_some() && it.open_question.is_none() {
                    it.open_question = Some(offset);
                    it.ask = Some(Ask {
                        options: Vec::new(),
                        default: None,
                        attempt: 0,
                        unclear: *unclear,
                    });
                }
            }
            WorkRecord::Accepted {
                item,
                tier,
                triage_skipped,
            } => {
                if rec.actor == Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                if let Some(rule) = triage_skipped {
                    // Only ingest, writing as the tracker, admits a root past
                    // triage, and only an imported root that has no children.
                    if rec.actor != Actor::Tracker {
                        return Err(Reject::ActorNotAllowed);
                    }
                    if rule.len() > MAX_RULE_NAME {
                        return Err(Reject::TooLong);
                    }
                    let it = self.item(item)?;
                    if it.parent.is_some() || !it.children.is_empty() {
                        return Err(Reject::NotARoot);
                    }
                }
                if tier.is_some_and(|t| t >= TIERS) {
                    return Err(Reject::BadTier);
                }
                let format = self.format();
                let it = self.item_mut(item)?;
                // A tier a person set while editing outranks the triage proposal.
                it.tier = tier
                    .or(it.tier)
                    .or(it.proposal.as_ref().map(|p| p.tier))
                    .or(Some(DEFAULT_TIER));
                it.accepted = true;
                it.triage_skipped.clone_from(triage_skipped);
                it.accepted_by_person = rec.actor == Actor::Person && format >= FORMAT_V6;
                // A proposal gets its root budget when a person accepts it, not before.
                if it.parent.is_none() {
                    self.budgets
                        .entry(item.clone())
                        .or_insert_with(Budget::for_new_root);
                }
                self.track(item);
                // Resume work an unaccepted item's answer made waits for this.
                let tier = self.item(item)?.tier;
                let waiting: Vec<ItemId> = self.item(item)?.children.clone();
                for c in waiting {
                    if let Some(c) = self.items.get_mut(&c)
                        && c.work == WorkKind::Resume
                    {
                        c.accepted = true;
                        c.tier = tier;
                    }
                }
            }
            WorkRecord::Prerequisite {
                item,
                attempt,
                prerequisite,
                key,
                new,
                done_when,
                triage,
                ..
            } => self.prerequisite(
                offset,
                rec,
                item,
                *attempt,
                prerequisite,
                key,
                new.as_ref(),
                *done_when,
                triage.as_ref(),
            )?,
            WorkRecord::Shipped { item, release } => {
                if rec.actor == Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                if release.is_empty() {
                    return Err(Reject::MissingField);
                }
                let it = self.item_mut(item)?;
                if !matches!(it.closed, Some(Closure::Merged | Closure::Completed)) {
                    return Err(Reject::NotClosed);
                }
                it.release = Some(release.clone());
            }
            WorkRecord::Linked { item, to } => {
                self.item(item)?;
                self.item(to)?;
                let scope = if rec.actor == Actor::Agent {
                    Some(self.agent_may_link(item, to, rec)?)
                } else {
                    None
                };
                if item == to || self.reaches(to, item) {
                    return Err(Reject::Cycle);
                }
                if let Some((worker, attempt)) = scope {
                    let p = self.path_mut(&worker, attempt)?;
                    if p.links >= MAX_LINKS {
                        return Err(Reject::LimitReached);
                    }
                    p.links += 1;
                }
                self.edges.insert((item.clone(), to.clone()));
            }
            WorkRecord::Unlinked { item, to } => {
                if rec.actor == Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                self.item(item)?;
                self.edges.remove(&(item.clone(), to.clone()));
                self.release_edges.remove(&(item.clone(), to.clone()));
            }
            WorkRecord::Context { item, entry, .. } => {
                self.item(item)?;
                if rec.actor == Actor::Agent {
                    let (worker, attempt) = self
                        .agent_scope(item, &rec.by, false)
                        .ok_or(Reject::NotHolder)?;
                    let p = self.path_mut(&worker, attempt)?;
                    if p.context_writes >= MAX_CONTEXT {
                        return Err(Reject::LimitReached);
                    }
                    p.context_writes += 1;
                }
                let it = self.item_mut(item)?;
                it.context.insert(entry.clone(), offset);
                if rec.actor == Actor::Agent {
                    it.context_agents.insert(entry.clone(), rec.by.clone());
                } else {
                    it.context_agents.remove(entry);
                }
            }
            WorkRecord::Claimed {
                item,
                attempt,
                deadline_ms,
                ..
            } => {
                if rec.actor != Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                let it = self.item(item)?;
                // A path that used its claims goes to Needs you; only a redo reopens the item.
                let retry = it.pending_fork.is_none() && !it.paths.is_empty();
                if retry
                    && it
                        .open_path()
                        .is_some_and(|p| !p.done && p.tries >= ATTEMPTS_ALLOWED)
                {
                    return Err(Reject::TooManyTries);
                }
                let root = it.root.clone();
                if self
                    .budgets
                    .get(&root)
                    .is_some_and(|b| b.cost_left_micros == 0)
                {
                    self.flag_over_budget(&root);
                    return Err(Reject::OverBudget);
                }
                let stage = self.stage(item).ok_or(Reject::UnknownItem)?;
                // A retry follows a release, lapse or give-up, which leave the item Ready.
                if stage != Stage::Ready {
                    return Err(Reject::NotReady);
                }
                // Only Ready fills the hold, so this keeps one holder per attempt.
                if let Some(Assignee::Agent(a)) = &self.items[item].assignee
                    && *a != rec.by
                {
                    return Err(Reject::NotAssignee);
                }
                if self.items[item].work == WorkKind::Review {
                    let root = self.items[item].root.clone();
                    if self.worked_on(&root, &rec.by) {
                        return Err(Reject::SelfReview);
                    }
                }
                self.holders.note(&rec.by, item);
                let it = self.item_mut(item)?;
                it.first_claim_ms.get_or_insert(rec.at_ms);
                let want = it.next_attempt();
                if *attempt != want {
                    return Err(Reject::WrongAttempt);
                }
                if retry {
                    if let Some(p) = it.paths.iter_mut().rev().find(|p| p.attempt == want) {
                        p.tries += 1;
                    }
                } else {
                    it.paths.push(Path {
                        attempt: *attempt,
                        tries: 1,
                        done: false,
                        closed: false,
                        forked_from: it.pending_fork.take(),
                        decisions: Vec::new(),
                        steps: Vec::new(),
                        edits: Vec::new(),
                        human_inputs: Vec::new(),
                        progress: 0,
                        context_writes: 0,
                        posts: 0,
                        triages: 0,
                        questions: 0,
                        answers: 0,
                        links: 0,
                        capped: false,
                        findings: 0,
                        proven: Vec::new(),
                        summary_accurate: None,
                    });
                    // A new attempt answers the asks made on the last one's request.
                    it.review_ask = None;
                    it.changes_ask = None;
                }
                if it.work != WorkKind::Review
                    && !it.workers.contains(&rec.by)
                    && it.workers.len() < MAX_WORKERS
                {
                    it.workers.push(rec.by.clone());
                }
                it.lapsed = None;
                it.hold = Some(Hold {
                    by: rec.by.clone(),
                    attempt: *attempt,
                    deadline_ms: *deadline_ms,
                    active_ms: rec.at_ms,
                });
            }
            WorkRecord::Progress {
                item,
                attempt,
                branch,
                pr,
                ..
            } => {
                self.acting_on(item, *attempt, rec)?;
                // One request, one item: another's request cannot be borrowed to close this one.
                if let Some(pr) = pr
                    && self
                        .items
                        .values()
                        .any(|o| o.id != *item && o.closed.is_none() && o.pr.as_ref() == Some(pr))
                {
                    return Err(Reject::PrInUse);
                }
                if rec.actor == Actor::Agent {
                    let p = self.path_mut(item, *attempt)?;
                    if p.progress >= MAX_PROGRESS {
                        return Err(Reject::LimitReached);
                    }
                    p.progress += 1;
                }
                let it = self.item_mut(item)?;
                if branch.is_some() {
                    it.branch = branch.clone();
                }
                if pr.is_some() {
                    // A status describes one request; a new one starts blank.
                    if it.pr != *pr {
                        it.pr_status = None;
                        it.red_since = None;
                        it.review_ask = None;
                        it.changes_ask = None;
                    }
                    it.pr = pr.clone();
                }
            }
            WorkRecord::Done { item, attempt, .. } => {
                let it = self.acting_on(item, *attempt, rec)?;
                if it.closed.is_some() {
                    return Err(Reject::Closed);
                }
                // Reviews of a pull request do not hold back its author's own completion.
                if self.open_children(item).iter().any(|c| {
                    self.items
                        .get(*c)
                        .is_some_and(|c| c.work != WorkKind::Review)
                }) {
                    return Err(Reject::OpenChildren);
                }
                if let Some(spec) = &it.review
                    && spec.pass == ReviewPass::Final
                    && !self.verdict_given(item)
                {
                    return Err(Reject::MissingField);
                }
                // Check the attempt before changing anything, so a refusal leaves no trace.
                let idx = it
                    .paths
                    .iter()
                    .position(|p| p.attempt == *attempt)
                    .ok_or(Reject::WrongAttempt)?;
                if it.paths[idx].closed {
                    return Err(Reject::Closed);
                }
                let root = it.root.clone();
                let it = self.item_mut(item)?;
                it.paths[idx].done = true;
                it.hold = None;
                // A fix waits for its merge; other work is finished when its attempt is.
                // A fix a review asked for lands in the request it came from.
                if it.work != WorkKind::Fix || it.finding.is_some() {
                    it.closed = Some(Closure::Completed);
                }
                self.track(item);
                self.charge(item, &root, rec);
            }
            WorkRecord::Released {
                item,
                attempt,
                reason,
                why,
                ..
            } => {
                let waits = why.is_some_and(ReleaseWhy::waits);
                if waits && self.late_writer(item, *attempt, rec)? {
                    self.park_wait(item, *attempt, rec, false)?;
                    let it = self.item_mut(item)?;
                    it.flagged = true;
                    return Ok(());
                }
                let it = self.acting_on(item, *attempt, rec)?;
                let Some(hold) = it.hold.clone() else {
                    return Err(Reject::NotHeld);
                };
                let root = it.root.clone();
                if waits {
                    self.park_wait(item, *attempt, rec, true)?;
                    // Not handed straight out again: a person moves it back to Ready.
                    self.item_mut(item)?.flagged = true;
                    return Ok(());
                }
                let lapse = self.format() >= FORMAT_V14
                    && rec.actor == Actor::Tracker
                    && reason.as_deref() == Some("lapsed");
                let it = self.item_mut(item)?;
                it.hold = None;
                if lapse {
                    it.lapsed = Some(hold);
                }
                self.charge(item, &root, rec);
            }
            WorkRecord::GaveUp { item, attempt, .. } => {
                let it = self.acting_on(item, *attempt, rec)?;
                if it.hold.is_none() {
                    return Err(Reject::NotHeld);
                }
                let root = it.root.clone();
                let it = self.item_mut(item)?;
                it.hold = None;
                self.charge(item, &root, rec);
            }
            WorkRecord::Question {
                item,
                attempt,
                options,
                default,
                ..
            } => {
                let late = self.late_writer(item, *attempt, rec)?;
                if !late {
                    self.agent_holder(item, *attempt, rec)?;
                }
                Self::check_options(options, default.as_deref())?;
                let p = self.path_mut(item, *attempt)?;
                if p.questions >= MAX_QUESTIONS {
                    return Err(Reject::LimitReached);
                }
                p.questions += 1;
                if self.format() >= FORMAT_V14 {
                    // The item waits in Needs you, so the agent is free for other work.
                    self.park_wait(item, *attempt, rec, !late)?;
                } else {
                    let root = self.item(item)?.root.clone();
                    self.charge(item, &root, rec);
                }
                let it = self.item_mut(item)?;
                it.open_question = Some(offset);
                it.ask = Some(Ask {
                    options: options.clone(),
                    default: default.clone(),
                    attempt: *attempt,
                    unclear: None,
                });
            }
            WorkRecord::Answer {
                item,
                question,
                evidence,
                chosen,
                then,
                ..
            } => {
                Self::check_evidence(offset, evidence)?;
                let scope = self.may_answer(item, rec)?;
                if self.item(item)?.open_question != Some(*question) {
                    return Err(Reject::NotOpenQuestion);
                }
                if let Some(c) = chosen {
                    let offered = self
                        .item(item)?
                        .ask
                        .as_ref()
                        .is_some_and(|a| a.options.contains(c));
                    if !offered {
                        return Err(Reject::BadOption);
                    }
                }
                self.check_then(item, then.as_ref(), rec)?;
                if let Some((worker, attempt)) = scope {
                    let p = self.path_mut(&worker, attempt)?;
                    if p.answers >= MAX_ANSWERS {
                        return Err(Reject::LimitReached);
                    }
                    p.answers += 1;
                }
                let v14 = self.format() >= FORMAT_V14;
                let it = self.item_mut(item)?;
                it.open_question = None;
                let ask = it.ask.take();
                // The asker let go when it asked, so the next claim reads the
                // answer from the item itself.
                if v14 && ask.as_ref().is_some_and(|a| a.unclear.is_none()) {
                    for (name, at) in [("question", *question), ("answer", offset)] {
                        if let Ok(entry) = Key::new(name) {
                            // Who wrote the entry decides who may rewrite it.
                            if rec.actor == Actor::Agent && name == "answer" {
                                it.context_agents.insert(entry.clone(), rec.by.clone());
                            } else {
                                it.context_agents.remove(&entry);
                            }
                            it.context.insert(entry, at);
                        }
                    }
                }
                self.follow_answer(offset, item, *question, ask, then.as_ref());
            }
            WorkRecord::Decision {
                item,
                attempt,
                so_far,
                commit,
                evidence,
                revert_check,
                ..
            } => {
                self.agent_holder(item, *attempt, rec)?;
                if so_far.is_empty() || commit.is_empty() {
                    return Err(Reject::MissingField);
                }
                Self::check_evidence(offset, evidence)?;
                if let Some(c) = revert_check {
                    if c.test.is_empty() {
                        return Err(Reject::MissingField);
                    }
                    if c.test.len() > MAX_REVERT_TEST {
                        return Err(Reject::TooLong);
                    }
                }
                let p = self.path_mut(item, *attempt)?;
                if p.decisions.len() >= MAX_DECISIONS {
                    return Err(Reject::LimitReached);
                }
                p.decisions.push(offset);
                if revert_check.as_ref().is_some_and(|c| c.proves()) {
                    p.proven.push(offset);
                }
            }
            WorkRecord::Finding {
                item,
                attempt,
                severity: _,
                file,
                line,
                title,
                scenario,
                verdict,
            } => self.finding(
                offset, rec, item, *attempt, file, *line, title, scenario, *verdict,
            )?,
            WorkRecord::ReviewVerdict {
                item,
                attempt,
                verdict,
                why,
            } => {
                self.agent_holder(item, *attempt, rec)?;
                let (spec, root) = self.final_review(item)?;
                if why.is_empty() {
                    return Err(Reject::MissingField);
                }
                if why.len() > MAX_VERDICT_WHY {
                    return Err(Reject::TooLong);
                }
                self.check_review_current(&root, &spec)?;
                if *verdict == MergeVerdict::Merge && self.open_fixes(&root) {
                    return Err(Reject::OpenChildren);
                }
                self.item_mut(&root)?.review_verdict = Some(VerdictNote {
                    verdict: *verdict,
                    review: item.clone(),
                    pr: spec.pr,
                    at: offset,
                });
            }
            WorkRecord::Step { item, attempt, .. } => {
                self.agent_holder(item, *attempt, rec)?;
                let p = self.path_mut(item, *attempt)?;
                if p.steps.len() >= MAX_STEPS {
                    return Err(Reject::LimitReached);
                }
                p.steps.push(offset);
            }
            WorkRecord::Edit {
                item,
                attempt,
                decision,
                ..
            } => {
                self.agent_holder(item, *attempt, rec)?;
                let p = self.path_mut(item, *attempt)?;
                if !p.decisions.contains(decision) {
                    return Err(Reject::UnknownDecision);
                }
                if p.edits.len() >= MAX_EDITS {
                    return Err(Reject::LimitReached);
                }
                p.edits.push(offset);
            }
            WorkRecord::HumanInput { item, attempt, .. } => {
                if rec.actor == Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                match attempt {
                    Some(a) => self.path_mut(item, *a)?.human_inputs.push(offset),
                    None => self.item_mut(item)?.human_inputs.push(offset),
                }
            }
            WorkRecord::ForkedFrom {
                item,
                from_attempt,
                at,
                note,
            } => {
                if rec.actor == Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                let it = self.item(item)?;
                if it.closed.is_some() {
                    return Err(Reject::Closed);
                }
                // A finished attempt can be redone while its item is unmerged; a running one cannot.
                if it.hold.is_some() || it.pending_fork.is_some() {
                    return Err(Reject::NotReady);
                }
                let p = it
                    .paths
                    .iter()
                    .find(|p| p.attempt == *from_attempt)
                    .ok_or(Reject::WrongAttempt)?;
                if !p.decisions.contains(at) {
                    return Err(Reject::UnknownDecision);
                }
                if let Some(n) = note {
                    let known = it.human_inputs.contains(n)
                        || it.paths.iter().any(|p| p.human_inputs.contains(n));
                    if !known {
                        return Err(Reject::UnknownInput);
                    }
                }
                if it.paths.iter().filter(|p| !p.closed).count() >= OPEN_ATTEMPTS {
                    return Err(Reject::TooManyOpen);
                }
                let limit = self
                    .budgets
                    .get(&it.root)
                    .map_or(DEFAULT_REDOS, |b| b.redos_per_item);
                if it.redos >= limit {
                    return Err(Reject::NoRedosLeft);
                }
                let it = self.item_mut(item)?;
                it.redos += 1;
                it.pending_fork = Some(Fork {
                    from_attempt: *from_attempt,
                    at: *at,
                    note: *note,
                });
            }
            WorkRecord::Closed { item, why } => {
                if rec.actor == Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                self.item(item)?;
                // Upstream sync closes as merged only the request the item reports, once merged.
                if *why == CloseWhy::Merged && rec.actor == Actor::Tracker {
                    let it = self.item(item)?;
                    let imported = self.upstream.values().any(|i| i == item);
                    let merged = it.pr_status.as_ref().is_some_and(|s| {
                        s.phase == PrPhase::Merged && it.pr.as_deref() == Some(s.pr.as_str())
                    });
                    if !imported || !merged {
                        return Err(Reject::NotMerged);
                    }
                }
                let how = match why {
                    CloseWhy::Merged => Closure::Merged,
                    CloseWhy::Completed => Closure::Completed,
                    CloseWhy::Dismissed => Closure::Dismissed,
                    CloseWhy::Upstream => Closure::Upstream,
                };
                let first = self.item(item)?.closed.is_none();
                self.close(item, how);
                if first && rec.actor == Actor::Tracker {
                    self.item_mut(item)?.closed_by_sync = true;
                }
            }
            WorkRecord::Rejected { item } => {
                if rec.actor == Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                if self.item(item)?.accepted {
                    return Err(Reject::NotAProposal);
                }
                self.close(item, Closure::Rejected);
            }
            WorkRecord::Updated { item, title } => {
                let it = self.upstream_sync(item, rec)?;
                if let Some(t) = title {
                    it.title.clone_from(t);
                }
                it.changed_upstream = true;
            }
            WorkRecord::LeftScope { item } => {
                let it = self.upstream_sync(item, rec)?;
                it.left_scope = true;
                it.left_scope_by_sync = rec.actor == Actor::Tracker;
            }
            WorkRecord::ReturnedToScope { item } => {
                let it = self.upstream_sync(item, rec)?;
                it.left_scope = false;
                it.left_scope_by_sync = false;
            }
            WorkRecord::Flagged { item, flagged } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                let it = self.item_mut(item)?;
                if it.closed.is_some() {
                    return Err(Reject::Closed);
                }
                // Refused rather than kept as a no-op, so "move it out" on an item
                // that waits for another reason says it did nothing. An item nobody
                // accepted waits for a person in Triage already.
                if it.flagged == *flagged || (*flagged && !it.accepted) {
                    return Err(Reject::NoChange);
                }
                it.flagged = *flagged;
            }
            WorkRecord::Retried { item } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                let it = self.item_mut(item)?;
                if it.closed.is_some() {
                    return Err(Reject::Closed);
                }
                // Only the item that stopped for want of tries: anything else
                // is not waiting on a retry.
                let stopped = it.hold.is_none() && it.pending_fork.is_none();
                let path = it.paths.iter_mut().rev().find(|p| !p.closed);
                match path {
                    Some(p) if stopped && !p.done && p.tries >= ATTEMPTS_ALLOWED => p.tries = 0,
                    _ => return Err(Reject::NoChange),
                }
            }
            WorkRecord::Snoozed { item, until_ms } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                if self.item(item)?.closed.is_some() {
                    return Err(Reject::Closed);
                }
                if *until_ms <= rec.at_ms || *until_ms - rec.at_ms > MAX_SNOOZE_MS {
                    return Err(Reject::BadTime);
                }
                // An item that waits on nobody is not in My queue to hide.
                if self.waiting_kind(item).is_none() {
                    return Err(Reject::NoChange);
                }
                let kind = self.waiting_kind(item);
                self.item_mut(item)?.snoozed = Some(Snooze {
                    at_ms: rec.at_ms,
                    until_ms: *until_ms,
                    kind,
                });
            }
            WorkRecord::Parked { item, parked } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                let it = self.item(item)?;
                if it.closed.is_some() {
                    return Err(Reject::Closed);
                }
                // A park covers only the wait it was set on, so one that has
                // ended no longer blocks a park on the next wait.
                let still_parked = it
                    .parked_at_ms
                    .is_some_and(|at| self.hide_covers(item, at, it.parked_kind));
                let refused = if *parked {
                    still_parked || self.waiting_kind(item).is_none()
                } else {
                    it.parked_at_ms.is_none()
                };
                if refused {
                    return Err(Reject::NoChange);
                }
                let kind = self.waiting_kind(item);
                let it = self.item_mut(item)?;
                it.parked_at_ms = parked.then_some(rec.at_ms);
                it.parked_kind = if *parked { kind } else { None };
            }
            WorkRecord::Unblocked { item, to } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                if self.item(item)?.closed.is_some() {
                    return Err(Reject::Closed);
                }
                // The same edge `unlinked` drops, but only one that still holds the item.
                if !self.is_linked(item, to) || !self.waits_on(item, to) {
                    return Err(Reject::NoChange);
                }
                self.edges.remove(&(item.clone(), to.clone()));
                self.release_edges.remove(&(item.clone(), to.clone()));
            }
            WorkRecord::ReviewRequested { item, reviewer } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                if reviewer.len() > MAX_REVIEWER {
                    return Err(Reject::TooLong);
                }
                if reviewer.trim().is_empty() || reviewer.chars().any(char::is_control) {
                    return Err(Reject::MissingField);
                }
                let it = self.item_mut(item)?;
                if it.closed.is_some() {
                    return Err(Reject::Closed);
                }
                // Only a request upstream shows open, and not one it shows approved.
                let pr = match (&it.pr, &it.pr_status) {
                    (Some(p), Some(s))
                        if *p == s.pr
                            && s.phase == PrPhase::Open
                            && s.review != PrReview::Approved =>
                    {
                        p.clone()
                    }
                    _ => return Err(Reject::NoChange),
                };
                let same = it
                    .review_ask
                    .as_ref()
                    .is_some_and(|a| a.pr == pr && a.reviewer == *reviewer);
                if same {
                    return Err(Reject::NoChange);
                }
                let review = it
                    .pr_status
                    .as_ref()
                    .map_or(PrReview::Unreviewed, |s| s.review);
                it.review_ask = Some(ReviewAsk {
                    reviewer: reviewer.clone(),
                    pr,
                    review,
                    at_ms: rec.at_ms,
                });
                // A new round of review: the earlier send-back has been taken up.
                it.changes_ask = None;
            }
            WorkRecord::ChangesRequested { item } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                let it = self.item_mut(item)?;
                if it.closed.is_some() {
                    return Err(Reject::Closed);
                }
                let pr = match (&it.pr, &it.pr_status) {
                    (Some(p), Some(s))
                        if *p == s.pr
                            && s.phase == PrPhase::Open
                            && s.review != PrReview::Approved =>
                    {
                        p.clone()
                    }
                    _ => return Err(Reject::NoChange),
                };
                if it.changes_ask.as_ref().is_some_and(|a| a.pr == pr) {
                    return Err(Reject::NoChange);
                }
                it.changes_ask = Some(ChangesAsk {
                    pr,
                    at_ms: rec.at_ms,
                });
                // The ask was answered.
                it.review_ask = None;
            }
            WorkRecord::MergeMarked { item } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                let it = self.item(item)?;
                if it.closed.is_some() {
                    return Err(Reject::Closed);
                }
                // Upstream may not have said yet, or may already say merged. A
                // request it shows open or closed unmerged was not merged.
                let seen = it
                    .pr_status
                    .as_ref()
                    .filter(|s| it.pr.as_deref() == Some(s.pr.as_str()))
                    .map(|s| s.phase);
                if it.pr.is_none() || matches!(seen, Some(PrPhase::Open | PrPhase::Closed)) {
                    return Err(Reject::NoChange);
                }
                self.close(item, Closure::Merged);
                self.item_mut(item)?.merge_marked = Some(MergeMark {
                    by: rec.by.clone(),
                    offset,
                });
            }
            WorkRecord::SummaryRated {
                item,
                attempt,
                accurate,
            } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                // A closed item's summary can still be marked: this judges what
                // the agent wrote, not the item.
                let p = self.path_mut(item, *attempt)?;
                if !p.done {
                    return Err(Reject::NotReady);
                }
                if p.summary_accurate == Some(*accurate) {
                    return Err(Reject::NoChange);
                }
                p.summary_accurate = Some(*accurate);
                self.mark_times.set(item, *attempt, rec.at_ms);
            }
            WorkRecord::Taken { item } => {
                let to = Some(Assignee::Person(rec.by.clone()));
                self.assign(item, to, rec)?;
            }
            WorkRecord::Reassigned { item, to } => {
                // Below 13 a reader may not route claims to an agent.
                if let Some(Assignee::Agent(a)) = to {
                    if self.format() < FORMAT_V13 {
                        return Err(Reject::BadAssignee);
                    }
                    // It could never claim its own tree's review, and nobody else may.
                    let it = self.item(item)?;
                    if it.work == WorkKind::Review && self.worked_on(&it.root, a) {
                        return Err(Reject::SelfReview);
                    }
                }
                // The tracker only lets go of an agent that stayed away.
                let away = rec.actor == Actor::Tracker
                    && to.is_none()
                    && self.items.get(item).is_some_and(|i| i.closed.is_none())
                    && matches!(
                        self.items.get(item).and_then(|i| i.assignee.as_ref()),
                        Some(Assignee::Agent(_))
                    );
                if away {
                    self.item_mut(item)?.assignee = None;
                } else {
                    self.assign(item, to.clone(), rec)?;
                }
            }
            WorkRecord::Steered { item, attempt, .. } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                let it = self.item(item)?;
                if it.closed.is_some() {
                    return Err(Reject::Closed);
                }
                // It names the attempt an agent holds, so it reaches that agent.
                match &it.hold {
                    None => return Err(Reject::NotHeld),
                    Some(h) if h.attempt != *attempt => return Err(Reject::WrongAttempt),
                    Some(_) => {}
                }
                self.path_mut(item, *attempt)?.human_inputs.push(offset);
            }
            WorkRecord::Reopened { item } => {
                if rec.actor == Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                let it = self.item_mut(item)?;
                if it.parent.is_some() {
                    return Err(Reject::NotARoot);
                }
                if it.closed.is_none() {
                    return Err(Reject::NotClosed);
                }
                it.closed = None;
                it.closed_by_sync = false;
                it.reopened_by = Some(rec.actor);
                // A new merge needs a new release.
                it.release = None;
                // Back to Triage: a person accepts it again.
                it.accepted = false;
                it.triage_skipped = None;
                it.accepted_by_person = false;
                self.track(item);
                let it = self.item_mut(item)?;
                it.left_scope = false;
                it.left_scope_by_sync = false;
                it.flagged = false;
                it.snoozed = None;
                it.parked_at_ms = None;
                it.parked_kind = None;
                it.review_ask = None;
                it.changes_ask = None;
                it.merge_marked = None;
                it.changed_upstream = true;
                it.proposal = None;
            }
            WorkRecord::AttemptClosed { item, attempt } => {
                if rec.actor == Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                let it = self.item_mut(item)?;
                let held = it.hold.as_ref().is_some_and(|h| h.attempt == *attempt);
                if held {
                    it.hold = None;
                }
                self.path_mut(item, *attempt)?.closed = true;
            }
            WorkRecord::PriceSet {
                model,
                unit,
                price_micros,
                effective_from_ms,
                as_of,
            } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                let from = effective_from_ms.unwrap_or(rec.at_ms);
                // A price never reaches back over usage already logged.
                if !model_name_ok(model)
                    || from < rec.at_ms
                    || as_of.as_deref().is_some_and(|d| !day_ok(d))
                {
                    return Err(Reject::BadPrice);
                }
                match as_of {
                    Some(day) => {
                        self.price_as_of
                            .entry(model.clone())
                            .or_default()
                            .entry(*unit)
                            .or_default()
                            .insert(from, day.clone());
                    }
                    // A later price at the same start replaces the note too.
                    None => {
                        if let Some(notes) = self
                            .price_as_of
                            .get_mut(model)
                            .and_then(|u| u.get_mut(unit))
                        {
                            notes.remove(&from);
                        }
                    }
                }
                let list = self
                    .prices
                    .entry(model.clone())
                    .or_default()
                    .entry(*unit)
                    .or_default();
                let at = list.partition_point(|(f, _)| *f <= from);
                list.insert(at, (from, *price_micros));
                // Only a price in effect now prices usage that waited for one.
                if from == rec.at_ms {
                    self.reprice(model, *unit, *price_micros);
                }
            }
            WorkRecord::BudgetSet {
                root,
                items_left,
                max_depth,
                cost_left_micros,
                redos_per_item,
            } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                let b = self.budgets.get_mut(root).ok_or(Reject::UnknownItem)?;
                *b = Budget {
                    items_left: *items_left,
                    max_depth: *max_depth,
                    cost_left_micros: *cost_left_micros,
                    redos_per_item: *redos_per_item,
                };
                self.item_mut(root)?.over_budget = false;
            }
            WorkRecord::RulesSet {
                target,
                root,
                text,
                checks,
            } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                if (text.is_none() && checks.is_none())
                    || target.source.is_empty()
                    || target.repo.is_empty()
                {
                    return Err(Reject::MissingField);
                }
                if text.as_ref().is_some_and(|t| t.len() > MAX_RULES_TEXT) {
                    return Err(Reject::LimitReached);
                }
                if let Some(c) = checks
                    && (c.len() > MAX_RULE_CHECKS
                        || c.iter().any(|x| x.is_empty() || x.len() > MAX_CHECK_BYTES))
                {
                    return Err(Reject::LimitReached);
                }
                if let Some(r) = root {
                    let it = self.item(r)?;
                    if it.parent.is_some() {
                        return Err(Reject::NotARoot);
                    }
                    if it.target.as_ref().is_some_and(|t| t != target) {
                        return Err(Reject::MissingField);
                    }
                }
                let set = RulesSet {
                    target: target.clone(),
                    root: root.clone(),
                    text: text.clone(),
                    checks: checks.clone(),
                    version: offset,
                };
                let same = |r: &RulesSet| {
                    r.root == set.root && (set.root.is_some() || r.target == set.target)
                };
                // Empty text and no checks clears the scope and frees its slot.
                let clears = set.text.as_deref() == Some("")
                    && set.checks.as_ref().is_none_or(Vec::is_empty);
                match self.rules.iter().position(same) {
                    Some(i) if clears => {
                        self.rules.remove(i);
                    }
                    Some(i) => self.rules[i] = set,
                    None if clears => {}
                    None if self.rules.len() >= MAX_RULE_SCOPES => {
                        return Err(Reject::LimitReached);
                    }
                    None => self.rules.push(set),
                }
            }
            WorkRecord::TierChanged { item, tier } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                if *tier >= TIERS {
                    return Err(Reject::BadTier);
                }
                let it = self.item_mut(item)?;
                if it.closed.is_some() {
                    return Err(Reject::Closed);
                }
                it.tier = Some(*tier);
            }
            WorkRecord::Edited {
                item,
                title,
                tier,
                depends_on,
                split,
                proposal,
            } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                if tier.is_some_and(|t| t >= TIERS) {
                    return Err(Reject::BadTier);
                }
                let it = self.item_mut(item)?;
                // Once accepted the item is in the queue; its tier changes with `tier_changed`.
                if it.accepted {
                    return Err(Reject::NotAProposal);
                }
                if it.closed.is_some() {
                    return Err(Reject::Closed);
                }
                if depends_on.is_some() || split.is_some() {
                    // The edit must name the answer it read, so it cannot land on a newer one.
                    if proposal.is_none() || it.proposal.as_ref().map(|p| p.at) != *proposal {
                        return Err(Reject::StaleProposal);
                    }
                    if let Some(deps) = depends_on {
                        let distinct: BTreeSet<_> = deps.iter().collect();
                        if distinct.len() != deps.len()
                            || deps.iter().any(|d| d == item || it.children.contains(d))
                        {
                            return Err(Reject::BadDependency);
                        }
                        if deps.iter().any(|d| !self.items.contains_key(d)) {
                            return Err(Reject::BadDependency);
                        }
                    }
                }
                let it = self.item_mut(item)?;
                if let Some(t) = title {
                    it.title = t.clone();
                }
                if tier.is_some() {
                    it.tier = *tier;
                }
                if let Some(p) = it.proposal.as_mut().filter(|_| proposal.is_some()) {
                    if let Some(deps) = depends_on {
                        p.depends_on.clone_from(deps);
                    }
                    if let Some(titles) = split {
                        p.split.clone_from(titles);
                    }
                }
            }
            WorkRecord::Refused { item, reason, .. } => {
                if rec.actor != Actor::Tracker {
                    return Err(Reject::ActorNotAllowed);
                }
                if let (Some(id), RefusalReason::Limit) = (item, reason) {
                    // Logged once per attempt, so a looping agent cannot fill the stream with refusals.
                    if let Some(attempt) = self
                        .items
                        .get(id)
                        .and_then(|i| i.hold.as_ref())
                        .map(|h| h.attempt)
                        && let Ok(p) = self.path_mut(id, attempt)
                    {
                        p.capped = true;
                    }
                }
                if let (Some(id), RefusalReason::Budget) = (item, reason)
                    && let Some(root) = self.items.get(id).map(|i| i.root.clone())
                {
                    self.item_mut(&root)?.over_budget = true;
                }
            }
            WorkRecord::PrStatus { item, status } => {
                // A person recorded the merge and upstream says otherwise: the
                // item comes back for a person to look at.
                if rec.actor != Actor::Agent
                    && status.phase != PrPhase::Merged
                    && status.fits()
                    && status.consistent()
                    && let Some(it) = self.items.get_mut(item)
                    && it.parent.is_none()
                    && matches!(it.closed, Some(Closure::Merged))
                    && it.merge_marked.is_some()
                    && it.pr.as_deref() == Some(status.pr.as_str())
                {
                    it.closed = None;
                    it.release = None;
                    self.track(item);
                }
                let it = self.upstream_sync(item, rec)?;
                if it.pr.as_deref() != Some(status.pr.as_str()) {
                    return Err(Reject::WrongPr);
                }
                if !status.fits() {
                    return Err(Reject::TooLong);
                }
                if !status.consistent() {
                    return Err(Reject::MissingField);
                }
                let red = |s: &PrSnapshot| s.phase == PrPhase::Open && s.checks == PrChecks::Red;
                if !red(status) {
                    it.red_since = None;
                } else if !it.pr_status.as_ref().is_some_and(red) {
                    it.red_since = Some(offset);
                }
                // A person's ask lasts while upstream shows the same request open
                // and has not answered it.
                let open = status.phase == PrPhase::Open;
                if it.review_ask.as_ref().is_some_and(|a| {
                    !open
                        || a.pr != status.pr
                        || status.review == PrReview::Approved
                        || (status.review == PrReview::ChangesRequested
                            && a.review != PrReview::ChangesRequested)
                }) {
                    it.review_ask = None;
                }
                if status.phase == PrPhase::Merged {
                    it.merge_marked = None;
                }
                if it.changes_ask.as_ref().is_some_and(|a| {
                    !open || a.pr != status.pr || status.review == PrReview::Approved
                }) {
                    it.changes_ask = None;
                }
                it.pr_status = Some(status.clone());
            }
            WorkRecord::SourceStalled {
                source,
                repo,
                status,
            } => {
                if rec.actor != Actor::Tracker {
                    return Err(Reject::ActorNotAllowed);
                }
                if source.len() > MAX_PR_REF || repo.len() > MAX_PR_REF {
                    return Err(Reject::TooLong);
                }
                let key = format!("{source}\0{repo}");
                match status {
                    Some(code) => {
                        if !self.source_stalls.contains_key(&key)
                            && self.source_stalls.len() >= MAX_STALLS
                        {
                            return Err(Reject::LimitReached);
                        }
                        self.source_stalls.insert(key, *code);
                    }
                    None => {
                        self.source_stalls.remove(&key);
                    }
                }
            }
            // Handled by the format gate.
            WorkRecord::FormatRaised { .. } | WorkRecord::Unknown => {}
        }
        Ok(())
    }

    /// Whether `by` claimed any item of the tree under `root` that is not a
    /// review or a triage: the author of the work a review reads.
    pub fn worked_on(&self, root: &ItemId, by: &Key) -> bool {
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let Some(it) = self.items.get(id) else {
                continue;
            };
            if !matches!(it.work, WorkKind::Review | WorkKind::Triage) && it.workers.contains(by) {
                return true;
            }
            stack.extend(it.children.iter());
        }
        false
    }

    /// Whether any fix a review asked for is still open under `root`.
    /// The final review of `pr` under `root`, if it is still unclaimed.
    fn open_final_review(&self, root: &ItemId, pr: &str) -> Option<ItemId> {
        let r = self.items.get(root)?;
        r.children.iter().find_map(|c| {
            let it = self.items.get(c)?;
            let spec = it.review.as_ref()?;
            (spec.pass == ReviewPass::Final
                && spec.pr == pr
                && it.closed.is_none()
                && it.hold.is_none())
            .then(|| c.clone())
        })
    }

    fn open_fixes(&self, root: &ItemId) -> bool {
        self.items.get(root).is_some_and(|r| {
            r.children.iter().any(|c| {
                self.items
                    .get(c)
                    .is_some_and(|c| c.finding.is_some() && c.closed.is_none())
            })
        })
    }

    /// Whether the root the final review `review` is under holds a verdict from it.
    fn verdict_given(&self, review: &ItemId) -> bool {
        let Some(it) = self.items.get(review) else {
            return false;
        };
        let (Some(spec), Some(root)) = (
            &it.review,
            it.parent.as_ref().and_then(|p| self.items.get(p)),
        ) else {
            return false;
        };
        root.review_verdict
            .as_ref()
            .is_some_and(|v| &v.review == review && v.pr == spec.pr)
    }

    /// The tracker adds a review under an open root that reports the request it reads.
    fn check_new_review(
        &self,
        rec: &WorkLogged,
        parent: Option<&ItemId>,
        spec: &ReviewSpec,
    ) -> Result<(), Reject> {
        if rec.actor != Actor::Tracker {
            return Err(Reject::ActorNotAllowed);
        }
        let root = parent
            .ok_or(Reject::MissingField)
            .and_then(|p| self.item(p))?;
        if root.parent.is_some() {
            return Err(Reject::NotARoot);
        }
        if root.closed.is_some() {
            return Err(Reject::Closed);
        }
        if !valid_review_tier(&spec.tier) || spec.pr.is_empty() {
            return Err(Reject::MissingField);
        }
        if spec.pr.len() > MAX_PR_REF {
            return Err(Reject::TooLong);
        }
        if root.pr.as_deref() != Some(spec.pr.as_str()) {
            return Err(Reject::WrongPr);
        }
        let reviews = root
            .children
            .iter()
            .filter(|c| self.items.get(*c).is_some_and(|c| c.review.is_some()))
            .count();
        if reviews >= MAX_REVIEWS_PER_ROOT {
            return Err(Reject::LimitReached);
        }
        Ok(())
    }

    /// The spec and root of a review item.
    fn review_of(&self, review: &ItemId) -> Result<(ReviewSpec, ItemId), Reject> {
        let it = self.item(review)?;
        let spec = it.review.clone().ok_or(Reject::NotReview)?;
        let root = it.parent.clone().ok_or(Reject::NotReview)?;
        Ok((spec, root))
    }

    fn final_review(&self, review: &ItemId) -> Result<(ReviewSpec, ItemId), Reject> {
        let (spec, root) = self.review_of(review)?;
        if spec.pass != ReviewPass::Final {
            return Err(Reject::NotReview);
        }
        Ok((spec, root))
    }

    /// A review speaks only for the request its root still reports, while open.
    fn check_review_current(&self, root: &ItemId, spec: &ReviewSpec) -> Result<(), Reject> {
        let r = self.item(root)?;
        if r.closed.is_some() {
            return Err(Reject::Closed);
        }
        if r.pr.as_deref() != Some(spec.pr.as_str()) {
            return Err(Reject::WrongPr);
        }
        Ok(())
    }

    /// One finding of a review. Every check runs before the first change.
    #[allow(clippy::too_many_arguments)]
    fn finding(
        &mut self,
        offset: u64,
        rec: &WorkLogged,
        review: &ItemId,
        attempt: u32,
        file: &str,
        line: u32,
        title: &str,
        scenario: &str,
        verdict: FindingVerdict,
    ) -> Result<(), Reject> {
        self.agent_holder(review, attempt, rec)?;
        let (spec, root) = self.review_of(review)?;
        if file.is_empty() || title.is_empty() || scenario.is_empty() || line == 0 {
            return Err(Reject::MissingField);
        }
        if file.len() > MAX_FINDING_FILE
            || title.len() > MAX_FINDING_TITLE
            || scenario.len() > MAX_SCENARIO
        {
            return Err(Reject::TooLong);
        }
        self.check_review_current(&root, &spec)?;
        let findings = self.path_mut(review, attempt)?.findings;
        if findings >= MAX_FINDINGS {
            return Err(Reject::LimitReached);
        }
        let child = |tag: &str| derived_id(tag, review, offset);
        let shorten = |prefix: &str| -> String {
            format!("{prefix}{title}")
                .chars()
                .take(MAX_FINDING_TITLE)
                .collect()
        };
        match verdict {
            FindingVerdict::Fix => {
                let reviews = self
                    .item(&root)?
                    .children
                    .iter()
                    .filter(|c| self.items.get(*c).is_some_and(|c| c.review.is_some()))
                    .count();
                if spec.pass == ReviewPass::Final
                    && self.open_final_review(&root, &spec.pr).is_none()
                    && reviews >= MAX_REVIEWS_PER_ROOT
                {
                    return Err(Reject::LimitReached);
                }
                // The fix is work the root's tree spends budget on, like any child.
                let r = self.item(&root)?;
                let (depth, tier) = (r.depth.saturating_add(1), r.tier);
                let b = self.budgets.get_mut(&root).ok_or(Reject::UnknownItem)?;
                let refused = if depth > b.max_depth {
                    Some(Reject::TooDeep)
                } else if b.items_left == 0 || b.cost_left_micros == 0 {
                    Some(Reject::OverBudget)
                } else {
                    b.items_left -= 1;
                    None
                };
                if let Some(why) = refused {
                    self.flag_over_budget(&root);
                    return Err(why);
                }
                let fix_id = child("fix");
                // The final review reads the tree with the fix in it, so it waits.
                // A first pass finds the final review already there. A final review
                // that asks for a fix is itself out of date once the fix lands, so it
                // gets a fresh one, a single one however many fixes it asks for.
                let waiting = match self.open_final_review(&root, &spec.pr) {
                    Some(last) => Some(last),
                    None if spec.pass == ReviewPass::Final => {
                        let last = child("final");
                        let mut again = Self::blank(
                            &last,
                            WorkKind::Review,
                            &format!("Final review: {}", spec.pr)
                                .chars()
                                .take(200)
                                .collect::<String>(),
                        );
                        again.review = Some(spec.clone());
                        again.parent = Some(root.clone());
                        again.root = root.clone();
                        again.depth = depth;
                        again.accepted = true;
                        self.new_item(again);
                        Some(last)
                    }
                    None => None,
                };
                if let Some(last) = waiting {
                    self.edges.insert((last, fix_id.clone()));
                }
                let mut fix = Self::blank(&fix_id, WorkKind::Fix, &shorten("Fix: "));
                fix.parent = Some(root.clone());
                fix.root = root;
                fix.depth = depth;
                fix.accepted = true;
                fix.tier = tier;
                fix.finding = Some(offset);
                fix.context.insert(finding_entry(), offset);
                self.new_item(fix);
            }
            FindingVerdict::Defer => {
                // Out of scope for this request: a person decides whether it is work.
                let target = self.item(&root)?.target.clone();
                let mut p = Self::blank(&child("defer"), WorkKind::Fix, &shorten("Deferred: "));
                p.proposed_by_agent = true;
                p.proposed_from = Some((rec.by.clone(), root.clone()));
                p.target = target;
                p.finding = Some(offset);
                p.context.insert(finding_entry(), offset);
                self.new_item(p);
            }
            FindingVerdict::Dismiss => {}
        }
        self.path_mut(review, attempt)?.findings += 1;
        self.item_mut(review)?.findings.push(offset);
        Ok(())
    }

    /// The open root an upstream sync record is about.
    fn upstream_sync(&mut self, id: &ItemId, rec: &WorkLogged) -> Result<&mut Item, Reject> {
        if rec.actor == Actor::Agent {
            return Err(Reject::ActorNotAllowed);
        }
        let it = self.item_mut(id)?;
        if it.parent.is_some() {
            return Err(Reject::NotARoot);
        }
        if it.closed.is_some() {
            return Err(Reject::Closed);
        }
        Ok(it)
    }

    fn path_mut(&mut self, item: &ItemId, attempt: u32) -> Result<&mut Path, Reject> {
        self.item_mut(item)?
            .paths
            .iter_mut()
            .find(|p| p.attempt == attempt)
            .ok_or(Reject::WrongAttempt)
    }

    #[allow(clippy::too_many_arguments)]
    fn propose(
        &mut self,
        rec: &WorkLogged,
        id: &ItemId,
        parent: Option<&ItemId>,
        key: &Key,
        work: WorkKind,
        title: &str,
        review: Option<&ReviewSpec>,
    ) -> Result<(), Reject> {
        if self.items.contains_key(id) {
            return Err(Reject::Duplicate);
        }
        if (work == WorkKind::Review) != review.is_some() {
            return Err(Reject::MissingField);
        }
        if let Some(spec) = review {
            self.check_new_review(rec, parent, spec)?;
        }
        // Proposals and roots are keyed like children, so a retry never duplicates.
        // Roots and agent proposals are scoped to the proposer, so two agents' keys never collide.
        let by = (parent.is_none() || (rec.actor == Actor::Agent && work == WorkKind::Fix))
            .then(|| rec.by.clone());
        let scope = (parent.cloned(), by, key.clone());
        if self.child_keys.contains(&scope) {
            return Err(Reject::Duplicate);
        }
        let mut item = Self::blank(id, work, title);
        item.review = review.cloned();
        match (rec.actor, parent) {
            // An agent's fix is out of scope for its task: a root once a person accepts it.
            (Actor::Agent, _) if work == WorkKind::Fix => {
                // The proposal counts against the attempt that found it.
                let (held, attempt) = self.held_by(rec.by.as_str()).ok_or(Reject::NoHeldParent)?;
                let p = self.path_mut(&held, attempt)?;
                if p.posts >= MAX_POSTS {
                    return Err(Reject::LimitReached);
                }
                let root = self.item(&held)?.root.clone();
                self.path_mut(&held, attempt)?.posts += 1;
                item.proposed_by_agent = true;
                item.proposed_from = Some((rec.by.clone(), root));
            }
            (Actor::Agent, Some(p)) if work.is_live_child() => {
                let par = self.item(p)?;
                match &par.hold {
                    Some(h) if h.by == rec.by => {}
                    _ => return Err(Reject::NoHeldParent),
                }
                let (root, depth) = (par.root.clone(), par.depth.saturating_add(1));
                let attempt = par.hold.as_ref().map_or(0, |h| h.attempt);
                if self.path_mut(p, attempt)?.posts >= MAX_POSTS {
                    return Err(Reject::LimitReached);
                }
                let b = self.budgets.get_mut(&root).ok_or(Reject::UnknownItem)?;
                let refused = if depth > b.max_depth {
                    Some(Reject::TooDeep)
                } else if b.items_left == 0 || b.cost_left_micros == 0 {
                    Some(Reject::OverBudget)
                } else {
                    b.items_left -= 1;
                    None
                };
                if let Some(why) = refused {
                    self.flag_over_budget(&root);
                    return Err(why);
                }
                self.path_mut(p, attempt)?.posts += 1;
                item.depth = depth;
                item.parent = Some(p.clone());
                item.root = root;
                item.accepted = true;
            }
            (Actor::Agent, _) => return Err(Reject::NoHeldParent),
            (_, Some(p)) => {
                // Items people and the tracker add draw nothing from the root budget,
                // but they nest no deeper than an agent could.
                let par = self.item(p)?;
                if par.closed.is_some() {
                    return Err(Reject::Closed);
                }
                let depth = par.depth.saturating_add(1);
                let max = self
                    .budgets
                    .get(&par.root)
                    .map_or(DEFAULT_MAX_DEPTH, |b| b.max_depth);
                if depth > max {
                    return Err(Reject::TooDeep);
                }
                item.depth = depth;
                item.root = par.root.clone();
                item.parent = Some(p.clone());
                item.accepted = true;
            }
            (actor, None) => {
                item.accepted = actor == Actor::Person;
                if item.accepted {
                    self.budgets.insert(id.clone(), Budget::for_new_root());
                }
            }
        }
        self.child_keys.insert(scope);
        self.new_item(item);
        Ok(())
    }

    /// Whether `by` already has as many proposals waiting for a person as it
    /// may, in all or from `root`. The tracker checks this before it appends a
    /// proposal; the fold only records where each came from, so an old log
    /// folds the same.
    pub fn proposals_full(&self, by: &Key, root: &ItemId) -> bool {
        let (mut mine, mut under_root) = (0, 0);
        for from in self
            .items
            .values()
            .filter(|i| !i.accepted && i.closed.is_none())
            .filter_map(|i| i.proposed_from.as_ref())
        {
            mine += usize::from(from.0 == *by);
            under_root += usize::from(from.1 == *root);
        }
        mine >= MAX_PENDING_PER_AGENT || under_root >= MAX_PENDING_PER_ROOT
    }

    /// Whether the proposals waiting from `root` already use up what is left
    /// of its budget, so one more would have nowhere to go if every one were
    /// accepted into the tree's work. A proposal takes nothing from the budget
    /// itself; a person's accept or reject leaves the count, so it frees the
    /// room. Checked by the tracker before it appends, so an old log folds the
    /// same.
    pub fn proposals_exceed_budget(&self, root: &ItemId) -> bool {
        let pending = self.waiting.per_root.get(root).copied().unwrap_or(0);
        self.budgets
            .get(root)
            .is_none_or(|b| b.cost_left_micros == 0 || pending >= b.items_left)
    }

    /// Whether the attempt on a triage item has logged as many different
    /// answers as it may.
    pub fn triage_full(&self, triage: &ItemId, attempt: u32) -> bool {
        self.items
            .get(triage)
            .and_then(|i| i.paths.iter().find(|p| p.attempt == attempt))
            .is_some_and(|p| p.triages >= MAX_TRIAGES)
    }

    /// The held item waits on a prerequisite, possibly one this record creates.
    /// Every check runs before the first change, so a refusal leaves no trace.
    #[allow(clippy::too_many_arguments)]
    fn prerequisite(
        &mut self,
        offset: u64,
        rec: &WorkLogged,
        item: &ItemId,
        attempt: u32,
        prerequisite: &ItemId,
        key: &Key,
        new: Option<&NewPrerequisite>,
        done_when: DoneWhen,
        triage: Option<&ItemId>,
    ) -> Result<(), Reject> {
        let holder = self
            .prerequisite_holder(item, attempt, triage, rec)?
            .clone();
        if self.path_mut(&holder, attempt)?.posts >= MAX_POSTS {
            return Err(Reject::LimitReached);
        }
        let scope = (None, Some(rec.by.clone()), key.clone());
        match new {
            Some(n) => {
                if n.title.is_empty() || n.target.repo.is_empty() || n.target.source.is_empty() {
                    return Err(Reject::MissingField);
                }
                Self::check_evidence(offset, &n.evidence)?;
                if self.items.contains_key(prerequisite) || self.child_keys.contains(&scope) {
                    return Err(Reject::Duplicate);
                }
            }
            None => {
                let p = self.item(prerequisite)?;
                // Finished but not yet released is still something to wait on.
                let unreleased = self.format() >= FORMAT_V5
                    && done_when == DoneWhen::Released
                    && matches!(p.closed, Some(Closure::Merged | Closure::Completed))
                    && p.release.is_none();
                if p.closed.is_some() && !unreleased {
                    return Err(Reject::Closed);
                }
            }
        }
        if item == prerequisite || triage == Some(prerequisite) || self.reaches(prerequisite, item)
        {
            return Err(Reject::Cycle);
        }
        if let Some(n) = new {
            self.new_prerequisite_item(offset, prerequisite, &n.target, &n.title);
            let root = self.item(item)?.root.clone();
            if let Some(p) = self.items.get_mut(prerequisite) {
                p.proposed_from = Some((rec.by.clone(), root));
            }
            self.track(prerequisite);
            self.child_keys.insert(scope);
        }
        self.edges.insert((item.clone(), prerequisite.clone()));
        let edge = (item.clone(), prerequisite.clone());
        if done_when == DoneWhen::Released {
            self.release_edges.insert(edge);
        } else if self.format() < FORMAT_V5 {
            // From format 5 the stricter of the two asks stands.
            self.release_edges.remove(&edge);
        }
        let root = self.item(item)?.root.clone();
        self.charge(&holder, &root, rec);
        let it = self.item_mut(&holder)?;
        // Triage still has its answer to give, so its claim goes on.
        if triage.is_none() {
            it.hold = None;
        }
        if let Some(p) = it.paths.iter_mut().find(|p| p.attempt == attempt) {
            if triage.is_none() {
                // Waiting on another repo is not a failed try.
                p.tries = p.tries.saturating_sub(1);
            }
            p.posts += 1;
        }
        Ok(())
    }

    /// From format 6, ties an open prerequisite to the issue ingest opened
    /// for it in its target repo, so upstream sync follows it like any
    /// imported root. Only a person's acceptance lets an issue be opened.
    fn attach_upstream(
        &mut self,
        item: &ItemId,
        source: &str,
        upstream_id: &str,
        up: String,
    ) -> Result<(), Reject> {
        if self.upstream.values().any(|i| i == item) {
            return Err(Reject::Duplicate);
        }
        let it = self.item(item)?;
        if it.parent.is_some() {
            return Err(Reject::NotARoot);
        }
        if it.closed.is_some() {
            return Err(Reject::Closed);
        }
        if !it.is_prerequisite() {
            return Err(Reject::NotAProposal);
        }
        if !it.accepted_by_person {
            return Err(Reject::ActorNotAllowed);
        }
        // GitHub names are case-insensitive, so a grant's casing may differ.
        let repo = upstream_id.split_once('#').map(|(repo, _)| repo);
        let same = it.target.as_ref().is_some_and(|t| {
            t.source == source && repo.is_some_and(|r| r.eq_ignore_ascii_case(&t.repo))
        });
        if !same {
            return Err(Reject::Duplicate);
        }
        self.upstream.insert(up, item.clone());
        Ok(())
    }

    /// The proposal a prerequisite creates in another repo's Triage. Its
    /// `prerequisite` context entry points at the record that made it.
    fn new_prerequisite_item(&mut self, offset: u64, id: &ItemId, target: &Target, title: &str) {
        let mut p = Self::blank(id, WorkKind::Fix, title);
        p.proposed_by_agent = true;
        p.target = Some(target.clone());
        if let Ok(entry) = Key::new("prerequisite") {
            p.context.insert(entry, offset);
        }
        self.new_item(p);
    }

    fn check_options(options: &[String], default: Option<&str>) -> Result<(), Reject> {
        if options.len() > MAX_ASK_OPTIONS {
            return Err(Reject::BadOption);
        }
        for (i, o) in options.iter().enumerate() {
            if o.is_empty() || options[..i].contains(o) {
                return Err(Reject::BadOption);
            }
            if o.len() > MAX_ASK_OPTION_LEN {
                return Err(Reject::TooLong);
            }
        }
        match default {
            Some(d) if !options.iter().any(|o| o == d) => Err(Reject::BadOption),
            _ => Ok(()),
        }
    }

    /// What an answer may lead to, checked before the answer changes anything.
    fn check_then(
        &self,
        item: &ItemId,
        then: Option<&Then>,
        rec: &WorkLogged,
    ) -> Result<(), Reject> {
        let Some(then) = then else {
            return Ok(());
        };
        let par = self.item(item)?;
        if par.closed.is_some() {
            return Err(Reject::Closed);
        }
        if matches!(then, Then::Resume) {
            let max = self
                .budgets
                .get(&par.root)
                .map_or(DEFAULT_MAX_DEPTH, |b| b.max_depth);
            if par.depth.saturating_add(1) > max {
                return Err(Reject::TooDeep);
            }
        }
        let Then::Prerequisite { target, title } = then else {
            return Ok(());
        };
        // A person decides where work goes. An agent that answers could steer
        // another repo's Triage, so it never may.
        if rec.actor != Actor::Person {
            return Err(Reject::ActorNotAllowed);
        }
        if title.is_empty() || target.source.is_empty() || target.repo.is_empty() {
            return Err(Reject::MissingField);
        }
        if title.len() > MAX_TITLE
            || target.source.len() > MAX_PR_REF
            || target.repo.len() > MAX_PR_REF
        {
            return Err(Reject::TooLong);
        }
        Ok(())
    }

    /// The work an answer starts: resume work under the asking item, or a
    /// prerequisite in another repo that the item waits on. The asker's
    /// hold ends either way, at no cost to its tries.
    fn follow_answer(
        &mut self,
        offset: u64,
        item: &ItemId,
        question: u64,
        ask: Option<Ask>,
        then: Option<&Then>,
    ) {
        let Some(then) = then else {
            return;
        };
        let attempt = ask.as_ref().map_or(0, |a| a.attempt);
        let Some(par) = self.items.get(item) else {
            return;
        };
        let id = |tag: &str| derived_id(tag, item, question);
        match then {
            Then::Resume => {
                let so_far = par
                    .paths
                    .iter()
                    .find(|p| p.attempt == attempt)
                    .and_then(|p| p.decisions.last().copied());
                let title: String = format!("Resume: {}", par.title).chars().take(200).collect();
                let mut r = Self::blank(&id("resume"), WorkKind::Resume, &title);
                r.parent = Some(item.clone());
                r.root = par.root.clone();
                r.depth = par.depth.saturating_add(1);
                // Work nobody accepted stays unclaimable until a person accepts the item.
                r.accepted = par.accepted;
                r.tier = par.tier;
                for (name, at) in [
                    ("question", Some(question)),
                    ("answer", Some(offset)),
                    ("so_far", so_far),
                ] {
                    if let (Ok(entry), Some(at)) = (Key::new(name), at) {
                        r.context.insert(entry, at);
                    }
                }
                self.new_item(r);
            }
            Then::Prerequisite { target, title } => {
                let prerequisite = id("move");
                self.new_prerequisite_item(offset, &prerequisite, target, title);
                self.edges.insert((item.clone(), prerequisite));
            }
        }
        if let Some(it) = self.items.get_mut(item)
            && it.hold.as_ref().is_some_and(|h| h.attempt == attempt)
        {
            it.hold = None;
            if let Some(p) = it.paths.iter_mut().find(|p| p.attempt == attempt) {
                p.tries = p.tries.saturating_sub(1);
            }
        }
    }

    /// Triage answers come from the agent holding the triage item for that target.
    /// With an agent, the triage item it holds and the open attempt.
    fn may_triage(
        &self,
        target: &ItemId,
        rec: &WorkLogged,
    ) -> Result<Option<(ItemId, u32)>, Reject> {
        self.item(target)?;
        if rec.actor != Actor::Agent {
            return Ok(None);
        }
        self.items
            .values()
            .filter(|t| t.work == WorkKind::Triage && t.parent.as_ref() == Some(target))
            .find_map(|t| {
                let h = t.hold.as_ref().filter(|h| h.by == rec.by)?;
                Some((t.id.clone(), h.attempt))
            })
            .map(Some)
            .ok_or(Reject::NotHolder)
    }

    /// The item `by` holds that lets it act on `id`: `id` itself or one of its
    /// children, or with `parent` its parent. The item's open attempt comes with it.
    fn agent_scope(&self, id: &ItemId, by: &Key, parent: bool) -> Option<(ItemId, u32)> {
        let it = self.items.get(id)?;
        let held = |i: &Item| {
            i.hold
                .as_ref()
                .filter(|h| h.by == *by && i.closed.is_none())
                .map(|h| (i.id.clone(), h.attempt))
        };
        held(it)
            .or_else(|| {
                it.children
                    .iter()
                    .filter_map(|c| self.items.get(c))
                    .find_map(held)
            })
            .or_else(|| {
                parent
                    .then(|| it.parent.as_ref().and_then(|p| self.items.get(p)))
                    .flatten()
                    .and_then(held)
            })
    }

    /// An agent adds an edge only inside one tree, on an item it works on or a
    /// sibling under the item it holds.
    fn agent_may_link(
        &self,
        item: &ItemId,
        to: &ItemId,
        rec: &WorkLogged,
    ) -> Result<(ItemId, u32), Reject> {
        if self.item(item)?.root != self.item(to)?.root {
            return Err(Reject::CrossRoot);
        }
        self.agent_scope(item, &rec.by, true)
            .ok_or(Reject::NotHolder)
    }

    /// Answers come from a person or the agent holding a question item under `item`.
    fn may_answer(&self, item: &ItemId, rec: &WorkLogged) -> Result<Option<(ItemId, u32)>, Reject> {
        let it = self.item(item)?;
        match rec.actor {
            Actor::Person => Ok(None),
            Actor::Tracker => Err(Reject::ActorNotAllowed),
            Actor::Agent => it
                .children
                .iter()
                .filter_map(|c| self.items.get(c))
                .filter(|q| q.work == WorkKind::Question)
                .find_map(|q| {
                    q.hold
                        .as_ref()
                        .filter(|h| h.by == rec.by)
                        .map(|h| (q.id.clone(), h.attempt))
                })
                .map(Some)
                .ok_or(Reject::NotHolder),
        }
    }

    /// Closes `item` as `how` and cancels everything open beneath it.
    fn close(&mut self, item: &ItemId, how: Closure) {
        let mut stack = vec![(item.clone(), how)];
        while let Some((id, how)) = stack.pop() {
            let Some(it) = self.items.get_mut(&id) else {
                continue;
            };
            if it.closed.is_some() {
                continue;
            }
            it.closed = Some(how);
            it.hold = None;
            it.pending_fork = None;
            stack.extend(it.children.iter().map(|c| (c.clone(), Closure::Cancelled)));
            self.track(&id);
        }
    }

    /// Whether `to` can be reached from `from` along edges.
    pub(crate) fn reaches(&self, from: &ItemId, to: &ItemId) -> bool {
        let mut seen = BTreeSet::new();
        let mut stack = vec![from];
        while let Some(n) = stack.pop() {
            if n == to {
                return true;
            }
            if seen.insert(n) {
                stack.extend(self.links_from(n));
            }
        }
        false
    }
}

/// The id of an item a record makes: the same on every replay.
fn derived_id(tag: &str, item: &ItemId, at: u64) -> ItemId {
    let hash = ArtifactId::of(format!("{tag}\n{item}\n{at}").as_bytes());
    ItemId::new(format!("w{}", &hash.as_str()[2..22])).expect("hash characters are valid")
}

/// The context entry that points an item at the finding it came from.
fn finding_entry() -> Key {
    Key::new("finding").expect("a valid key")
}
