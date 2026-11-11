//! The fold from a prefix of `work.events` to item state.
//!
//! Pure: no clock, no I/O, no model. [`State::apply`] takes one record and
//! either changes the state or returns why it did not. A rejected record is
//! kept in [`State::rejections`], which is the validation view: what a record
//! would have done had the tracker let it through. The live tracker and a
//! replay run the same code, so they agree by construction.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::names::{ItemId, Key};
use crate::work::{
    Actor, CloseWhy, DoneWhen, Evidence, NewPrerequisite, RefusalReason, TIERS, Target,
    WORK_EVENTS, WorkKind, WorkLogged, WorkRecord,
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
/// Children and proposals together.
const MAX_POSTS: u32 = 24;

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
    pub at: u64,
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
    /// The upstream issue changed after the last triage answer.
    pub changed_upstream: bool,
    /// The upstream issue no longer matches the rule that admitted it.
    pub left_scope: bool,
    pub accepted: bool,
    pub tier: Option<u8>,
    pub proposal: Option<Proposal>,
    pub hold: Option<Hold>,
    pub closed: Option<Closure>,
    pub redos: u8,
    pub over_budget: bool,
    pub open_question: Option<u64>,
    pub branch: Option<String>,
    pub pr: Option<String>,
    pub cost_micros: u64,
    pub context: BTreeMap<Key, u64>,
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
    pub edge_done: BTreeMap<(ItemId, ItemId), DoneWhen>,
    pub budgets: BTreeMap<ItemId, Budget>,
    pub rejections: Vec<Rejection>,
    upstream: BTreeMap<String, ItemId>,
    child_keys: BTreeSet<(Option<ItemId>, Option<Key>, Key)>,
    /// Roots flagged over budget while a trial is running, to unflag on a refusal.
    #[serde(skip)]
    probing: Option<Vec<ItemId>>,
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
                .any(|(a, b)| a == id && self.waits_on(a, b)) =>
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
            return Some(NeedsYou::Question);
        }
        if it.over_budget {
            return Some(NeedsYou::OverBudget);
        }
        // A holder may finish or release first; only then does the item stop.
        if it.left_scope && it.hold.is_none() {
            return Some(NeedsYou::LeftScope);
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
                dep.release.is_none()
                    && self.edge_done.get(&(from.clone(), to.clone())) == Some(&DoneWhen::Released)
            }
            Some(_) => false,
        }
    }

    fn is_closed(&self, id: &ItemId) -> bool {
        self.items.get(id).is_some_and(|i| i.closed.is_some())
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
            changed_upstream: false,
            left_scope: false,
            accepted: false,
            tier: None,
            proposal: None,
            hold: None,
            closed: None,
            redos: 0,
            over_budget: false,
            open_question: None,
            branch: None,
            pr: None,
            cost_micros: 0,
            context: BTreeMap::new(),
            paths: Vec::new(),
            human_inputs: Vec::new(),
            pending_fork: None,
            target: None,
            release: None,
        }
    }

    fn draw_cost(&mut self, root: &ItemId, micros: u64) {
        if let Some(b) = self.budgets.get_mut(root) {
            b.cost_left_micros = b.cost_left_micros.saturating_sub(micros);
        }
    }

    fn step(&mut self, offset: u64, rec: &WorkLogged) -> Result<(), Reject> {
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
            } => self.propose(rec, item, parent.as_ref(), key, *work, title)?,
            WorkRecord::Triaged {
                item,
                tier,
                why,
                duplicate_of,
            } => {
                if *tier >= TIERS {
                    return Err(Reject::BadTier);
                }
                self.may_triage(item, rec)?;
                self.item_mut(item)?.proposal = Some(Proposal {
                    tier: *tier,
                    why: why.clone(),
                    duplicate_of: duplicate_of.clone(),
                    at: offset,
                });
                self.item_mut(item)?.changed_upstream = false;
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
            }
            WorkRecord::Prerequisite {
                item,
                attempt,
                prerequisite,
                key,
                new,
                done_when,
            } => self.prerequisite(
                offset,
                rec,
                item,
                *attempt,
                prerequisite,
                key,
                new.as_ref(),
                *done_when,
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
                self.edge_done.remove(&(item.clone(), to.clone()));
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
                self.item_mut(item)?.context.insert(entry.clone(), offset);
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
                let it = self.item_mut(item)?;
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
                        questions: 0,
                        answers: 0,
                        links: 0,
                        capped: false,
                    });
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
                if !self.open_children(item).is_empty() {
                    return Err(Reject::OpenChildren);
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
                it.cost_micros += cost_micros;
                // A fix waits for its merge; other work is finished when its attempt is.
                if it.work != WorkKind::Fix {
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
                it.cost_micros += cost_micros;
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
                it.cost_micros += cost_micros;
                self.draw_cost(&root, *cost_micros);
            }
            WorkRecord::Question { item, attempt, .. } => {
                self.agent_holder(item, *attempt, rec)?;
                let p = self.path_mut(item, *attempt)?;
                if p.questions >= MAX_QUESTIONS {
                    return Err(Reject::LimitReached);
                }
                p.questions += 1;
                self.item_mut(item)?.open_question = Some(offset);
            }
            WorkRecord::Answer {
                item,
                question,
                evidence,
                ..
            } => {
                Self::check_evidence(offset, evidence)?;
                let scope = self.may_answer(item, rec)?;
                if self.item(item)?.open_question != Some(*question) {
                    return Err(Reject::NotOpenQuestion);
                }
                if let Some((worker, attempt)) = scope {
                    let p = self.path_mut(&worker, attempt)?;
                    if p.answers >= MAX_ANSWERS {
                        return Err(Reject::LimitReached);
                    }
                    p.answers += 1;
                }
                self.item_mut(item)?.open_question = None;
            }
            WorkRecord::Decision {
                item,
                attempt,
                so_far,
                commit,
                evidence,
                ..
            } => {
                self.agent_holder(item, *attempt, rec)?;
                if so_far.is_empty() || commit.is_empty() {
                    return Err(Reject::MissingField);
                }
                Self::check_evidence(offset, evidence)?;
                let p = self.path_mut(item, *attempt)?;
                if p.decisions.len() >= MAX_DECISIONS {
                    return Err(Reject::LimitReached);
                }
                p.decisions.push(offset);
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
                let how = match why {
                    CloseWhy::Merged => Closure::Merged,
                    CloseWhy::Completed => Closure::Completed,
                    CloseWhy::Dismissed => Closure::Dismissed,
                    CloseWhy::Upstream => Closure::Upstream,
                };
                self.close(item, how);
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
                self.upstream_sync(item, rec)?.left_scope = true;
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
                // Back to Triage: a person accepts it again.
                it.accepted = false;
                it.left_scope = false;
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
            WorkRecord::Unknown => {}
        }
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

    fn propose(
        &mut self,
        rec: &WorkLogged,
        id: &ItemId,
        parent: Option<&ItemId>,
        key: &Key,
        work: WorkKind,
        title: &str,
    ) -> Result<(), Reject> {
        if self.items.contains_key(id) {
            return Err(Reject::Duplicate);
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
        match (rec.actor, parent) {
            // An agent's fix is out of scope for its task: a root once a person accepts it.
            (Actor::Agent, _) if work == WorkKind::Fix => {
                // The proposal counts against the attempt that found it.
                let (held, attempt) = self.held_by(rec.by.as_str()).ok_or(Reject::NoHeldParent)?;
                let p = self.path_mut(&held, attempt)?;
                if p.posts >= MAX_POSTS {
                    return Err(Reject::LimitReached);
                }
                p.posts += 1;
                item.proposed_by_agent = true;
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
            let mut p = Self::blank(prerequisite, WorkKind::Fix, &n.title);
            p.proposed_by_agent = true;
            p.target = Some(n.target.clone());
            if let Ok(entry) = Key::new("prerequisite") {
                p.context.insert(entry, offset);
            }
            self.child_keys.insert(scope);
            self.new_item(p);
        }
        self.edges.insert((item.clone(), prerequisite.clone()));
        self.edge_done
            .insert((item.clone(), prerequisite.clone()), done_when);
        let it = self.item_mut(item)?;
        it.hold = None;
        if let Some(p) = it.paths.iter_mut().find(|p| p.attempt == attempt) {
            // Waiting on another repo is not a failed try.
            p.tries = p.tries.saturating_sub(1);
            p.posts += 1;
        }
        Ok(())
    }

    /// Triage answers come from the agent holding the triage item for that target.
    fn may_triage(&self, target: &ItemId, rec: &WorkLogged) -> Result<(), Reject> {
        self.item(target)?;
        if rec.actor != Actor::Agent {
            return Ok(());
        }
        let holds = self.items.values().any(|t| {
            t.work == WorkKind::Triage
                && t.parent.as_ref() == Some(target)
                && t.hold.as_ref().is_some_and(|h| h.by == rec.by)
        });
        if holds {
            Ok(())
        } else {
            Err(Reject::NotHolder)
        }
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
