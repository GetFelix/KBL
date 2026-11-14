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
    Actor, CloseWhy, DoneWhen, Evidence, FORMAT, FORMAT_V1, FindingVerdict, MAX_ASK_OPTION_LEN,
    MAX_ASK_OPTIONS, MAX_CHECK_BYTES, MAX_FINDING_FILE, MAX_FINDING_TITLE, MAX_FINDINGS,
    MAX_PR_REF, MAX_REVERT_TEST, MAX_REVIEWS_PER_ROOT, MAX_RULE_CHECKS, MAX_RULE_SCOPES,
    MAX_RULES_TEXT, MAX_SCENARIO, MAX_TITLE, MAX_VERDICT_WHY, MergeVerdict, NewPrerequisite,
    PrChecks, PrPhase, PrReview, PrSnapshot, RefusalReason, ReviewPass, ReviewSpec, TIERS, Target,
    Then, Unclear, WORK_EVENTS, WorkKind, WorkLogged, WorkRecord, valid_review_tier,
};

#[cfg(test)]
mod tests;

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

    /// The newest attempt that is not closed.
    pub fn open_path(&self) -> Option<&Path> {
        self.paths.iter().rev().find(|p| !p.closed)
    }
}

/// Who holds an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    pub by: Key,
    pub attempt: u32,
    pub deadline_ms: u64,
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
}

fn is_false(b: &bool) -> bool {
    !*b
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
    pub accepted: bool,
    pub tier: Option<u8>,
    pub proposal: Option<Proposal>,
    pub hold: Option<Hold>,
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
    /// When the first claim on the item was logged, in the tracker's clock.
    #[serde(default)]
    pub first_claim_ms: Option<u64>,
    pub cost_micros: u64,
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
    /// Roots flagged over budget while a trial is running, to unflag on a refusal.
    #[serde(skip)]
    probing: Option<Vec<ItemId>>,
}

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
        if let Err(why) = self.step(offset, rec) {
            self.rejections.push(Rejection { offset, why });
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
            Ok(()) => self.at = Some(offset),
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

    /// The open item `agent` holds, with the attempt, if any.
    pub fn held_by(&self, agent: &str) -> Option<(ItemId, u32)> {
        self.items.values().find_map(|i| {
            let h = i.hold.as_ref()?;
            (i.closed.is_none() && h.by.as_str() == agent).then(|| (i.id.clone(), h.attempt))
        })
    }

    /// Every imported root with its source and upstream id.
    pub fn imported(&self) -> impl Iterator<Item = (&str, &str, &ItemId)> {
        self.upstream.iter().filter_map(|(up, item)| {
            let (source, id) = up.split_once('\n')?;
            Some((source, id, item))
        })
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
            None if it.pending_fork.is_none() && it.open_path().is_some_and(|p| p.done) => {
                Stage::InReview
            }
            None if self
                .edges
                .iter()
                .any(|(a, b)| a == id && self.waits_on(a, b))
                || self.resuming(id) =>
            {
                Stage::Blocked
            }
            None => Stage::Ready,
        })
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
        // A holder may finish or release first; only then does the item stop.
        if it.left_scope && it.hold.is_none() {
            return Some(NeedsYou::LeftScope);
        }
        if it.hold.is_none()
            && it
                .pr_status
                .as_ref()
                .is_some_and(|s| s.phase == PrPhase::Open && s.checks == PrChecks::Red)
        {
            return Some(NeedsYou::CheckFailed);
        }
        let dropped = self.edges.iter().any(|(a, b)| {
            a == id
                && self.items.get(b).is_some_and(|d| {
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
        None
    }

    /// The wording an open pull request gets on the board, if it has one.
    /// A red check outranks both: that item is in Needs you.
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

    /// Whether a link says `from` waits on `to`, open or not.
    pub fn is_linked(&self, from: &ItemId, to: &ItemId) -> bool {
        self.edges.contains(&(from.clone(), to.clone()))
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
            WorkRecord::Decision { item, attempt, .. }
            | WorkRecord::Prerequisite { item, attempt, .. } => {
                self.agent_holder(item, *attempt, rec)
            }
            WorkRecord::Answer { item, .. } => self.may_answer(item, rec).map(|_| ()),
            _ => Ok(()),
        }
    }

    fn agent_holder(&self, id: &ItemId, attempt: u32, rec: &WorkLogged) -> Result<(), Reject> {
        if rec.actor != Actor::Agent {
            return Err(Reject::ActorNotAllowed);
        }
        self.acting_on(id, attempt, rec).map(|_| ())
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
        self.items.insert(item.id.clone(), item);
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
            left_scope_by_sync: false,
            accepted: false,
            tier: None,
            proposal: None,
            hold: None,
            closed: None,
            closed_by_sync: false,
            reopened_by: None,
            redos: 0,
            over_budget: false,
            open_question: None,
            ask: None,
            branch: None,
            pr: None,
            pr_status: None,
            first_claim_ms: None,
            cost_micros: 0,
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

    /// The highest format a record on this log may have. A `format_raised`
    /// record raises it.
    pub fn format(&self) -> u32 {
        self.format.unwrap_or(FORMAT_V1)
    }

    fn step(&mut self, offset: u64, rec: &WorkLogged) -> Result<(), Reject> {
        if let Some(raised) = format_gate(FORMAT, self.format(), rec)? {
            self.format = Some(raised);
            return Ok(());
        }
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
                if self.items.contains_key(item) || self.upstream.contains_key(&up) {
                    return Err(Reject::Duplicate);
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
            WorkRecord::Accepted { item, tier } => {
                if rec.actor == Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                if tier.is_some_and(|t| t >= TIERS) {
                    return Err(Reject::BadTier);
                }
                let it = self.item_mut(item)?;
                // A tier a person set while editing outranks the triage proposal.
                it.tier = tier
                    .or(it.tier)
                    .or(it.proposal.as_ref().map(|p| p.tier))
                    .or(Some(DEFAULT_TIER));
                it.accepted = true;
                // A proposal gets its root budget when a person accepts it, not before.
                if it.parent.is_none() {
                    self.budgets
                        .entry(item.clone())
                        .or_insert_with(Budget::for_new_root);
                }
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
                cost_micros,
            } => self.prerequisite(
                offset,
                rec,
                item,
                *attempt,
                prerequisite,
                key,
                new.as_ref(),
                *done_when,
                *cost_micros,
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
                if self.items[item].work == WorkKind::Review {
                    let root = self.items[item].root.clone();
                    if self.worked_on(&root, &rec.by) {
                        return Err(Reject::SelfReview);
                    }
                }
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
                    });
                }
                if it.work != WorkKind::Review
                    && !it.workers.contains(&rec.by)
                    && it.workers.len() < MAX_WORKERS
                {
                    it.workers.push(rec.by.clone());
                }
                it.hold = Some(Hold {
                    by: rec.by.clone(),
                    attempt: *attempt,
                    deadline_ms: *deadline_ms,
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
                    }
                    it.pr = pr.clone();
                }
            }
            WorkRecord::Done {
                item,
                attempt,
                cost_micros,
            } => {
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
                it.cost_micros = it.cost_micros.saturating_add(*cost_micros);
                // A fix waits for its merge; other work is finished when its attempt is.
                // A fix a review asked for lands in the request it came from.
                if it.work != WorkKind::Fix || it.finding.is_some() {
                    it.closed = Some(Closure::Completed);
                }
                self.draw_cost(&root, *cost_micros);
            }
            WorkRecord::Released {
                item,
                attempt,
                cost_micros,
                ..
            } => {
                let it = self.acting_on(item, *attempt, rec)?;
                if it.hold.is_none() {
                    return Err(Reject::NotHeld);
                }
                let root = it.root.clone();
                let it = self.item_mut(item)?;
                it.hold = None;
                it.cost_micros = it.cost_micros.saturating_add(*cost_micros);
                self.draw_cost(&root, *cost_micros);
            }
            WorkRecord::GaveUp {
                item,
                attempt,
                cost_micros,
                ..
            } => {
                let it = self.acting_on(item, *attempt, rec)?;
                if it.hold.is_none() {
                    return Err(Reject::NotHeld);
                }
                let root = it.root.clone();
                let it = self.item_mut(item)?;
                it.hold = None;
                it.cost_micros = it.cost_micros.saturating_add(*cost_micros);
                self.draw_cost(&root, *cost_micros);
            }
            WorkRecord::Question {
                item,
                attempt,
                options,
                default,
                cost_micros,
                ..
            } => {
                self.agent_holder(item, *attempt, rec)?;
                Self::check_options(options, default.as_deref())?;
                let p = self.path_mut(item, *attempt)?;
                if p.questions >= MAX_QUESTIONS {
                    return Err(Reject::LimitReached);
                }
                p.questions += 1;
                let root = self.item(item)?.root.clone();
                self.draw_cost(&root, *cost_micros);
                let it = self.item_mut(item)?;
                it.cost_micros = it.cost_micros.saturating_add(*cost_micros);
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
                let it = self.item_mut(item)?;
                it.open_question = None;
                let ask = it.ask.take();
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
                it.left_scope = false;
                it.left_scope_by_sync = false;
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
            WorkRecord::Edited { item, title, tier } => {
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
                if let Some(t) = title {
                    it.title = t.clone();
                }
                if tier.is_some() {
                    it.tier = *tier;
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
        cost_micros: u64,
    ) -> Result<(), Reject> {
        self.agent_holder(item, attempt, rec)?;
        if self.path_mut(item, attempt)?.posts >= MAX_POSTS {
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
                if self.item(prerequisite)?.closed.is_some() {
                    return Err(Reject::Closed);
                }
            }
        }
        if item == prerequisite || self.reaches(prerequisite, item) {
            return Err(Reject::Cycle);
        }
        if let Some(n) = new {
            self.new_prerequisite_item(offset, prerequisite, &n.target, &n.title);
            let root = self.item(item)?.root.clone();
            if let Some(p) = self.items.get_mut(prerequisite) {
                p.proposed_from = Some((rec.by.clone(), root));
            }
            self.child_keys.insert(scope);
        }
        self.edges.insert((item.clone(), prerequisite.clone()));
        let edge = (item.clone(), prerequisite.clone());
        if done_when == DoneWhen::Released {
            self.release_edges.insert(edge);
        } else {
            self.release_edges.remove(&edge);
        }
        let root = self.item(item)?.root.clone();
        self.draw_cost(&root, cost_micros);
        let it = self.item_mut(item)?;
        it.hold = None;
        it.cost_micros = it.cost_micros.saturating_add(cost_micros);
        if let Some(p) = it.paths.iter_mut().find(|p| p.attempt == attempt) {
            // Waiting on another repo is not a failed try.
            p.tries = p.tries.saturating_sub(1);
            p.posts += 1;
        }
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
        }
    }

    /// Whether `to` can be reached from `from` along edges.
    fn reaches(&self, from: &ItemId, to: &ItemId) -> bool {
        let mut seen = BTreeSet::new();
        let mut stack = vec![from];
        while let Some(n) = stack.pop() {
            if n == to {
                return true;
            }
            if seen.insert(n) {
                stack.extend(self.edges.iter().filter(|(a, _)| a == n).map(|(_, b)| b));
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

/// Snapshots taken every `every` records, so a seek replays a short tail
/// instead of the whole stream.
pub struct Seek {
    every: usize,
    /// `(records applied, state after them)`.
    snaps: Vec<(usize, State)>,
}

impl Seek {
    pub fn build(log: &[(u64, WorkLogged)], every: usize) -> Seek {
        let every = every.max(1);
        let mut state = State::default();
        let mut snaps = Vec::new();
        for (i, (offset, rec)) in log.iter().enumerate() {
            state.apply(*offset, rec);
            if (i + 1) % every == 0 {
                snaps.push((i + 1, state.clone()));
            }
        }
        Seek { every, snaps }
    }

    pub fn every(&self) -> usize {
        self.every
    }

    /// The state with every record at or before `offset` applied and none after.
    /// `log` is the slice the seek was built from, offsets ascending.
    pub fn state_at(&self, log: &[(u64, WorkLogged)], offset: u64) -> State {
        let end = log.partition_point(|(o, _)| *o <= offset);
        let snap = self.snaps.iter().rev().find(|(n, _)| *n <= end);
        let (start, mut state) = match snap {
            Some((n, s)) => (*n, s.clone()),
            None => (0, State::default()),
        };
        for (o, rec) in &log[start..end] {
            state.apply(*o, rec);
        }
        state
    }
}
