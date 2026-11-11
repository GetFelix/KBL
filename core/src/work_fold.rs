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
    Actor, CloseWhy, Evidence, RefusalReason, WORK_EVENTS, WorkKind, WorkLogged, WorkRecord,
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
}

impl Item {
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
    pub budgets: BTreeMap<ItemId, Budget>,
    pub rejections: Vec<Rejection>,
    upstream: BTreeMap<String, ItemId>,
    child_keys: BTreeSet<(Option<ItemId>, Option<Key>, Key)>,
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
                .any(|(a, b)| a == id && !self.is_closed(b)) =>
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

    fn is_closed(&self, id: &ItemId) -> bool {
        self.items.get(id).is_some_and(|i| i.closed.is_some())
    }

    /// Marks the root over budget so the stage does not depend on a later `refused`.
    fn flag_over_budget(&mut self, root: &ItemId) {
        if let Some(r) = self.items.get_mut(root) {
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
                self.new_item(Self::blank(item, WorkKind::Fix, title));
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
                self.may_triage(item, rec)?;
                self.item_mut(item)?.proposal = Some(Proposal {
                    tier: *tier,
                    why: why.clone(),
                    duplicate_of: duplicate_of.clone(),
                    at: offset,
                });
            }
            WorkRecord::Accepted { item, tier } => {
                if rec.actor == Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                let it = self.item_mut(item)?;
                it.tier = tier
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
            WorkRecord::Linked { item, to } => {
                self.item(item)?;
                self.item(to)?;
                if item == to || self.reaches(to, item) {
                    return Err(Reject::Cycle);
                }
                self.edges.insert((item.clone(), to.clone()));
            }
            WorkRecord::Unlinked { item, to } => {
                if rec.actor == Actor::Agent {
                    return Err(Reject::ActorNotAllowed);
                }
                self.item(item)?;
                self.edges.remove(&(item.clone(), to.clone()));
            }
            WorkRecord::Context { item, entry, .. } => {
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
                let want = if retry {
                    it.open_path().map_or(1, |p| p.attempt)
                } else {
                    it.paths.last().map_or(1, |p| p.attempt + 1)
                };
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
                self.item_mut(item)?.open_question = Some(offset);
            }
            WorkRecord::Answer {
                item,
                question,
                evidence,
                ..
            } => {
                Self::check_evidence(offset, evidence)?;
                self.may_answer(item, rec)?;
                let it = self.item_mut(item)?;
                if it.open_question == Some(*question) {
                    it.open_question = None;
                }
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
                self.path_mut(item, *attempt)?.decisions.push(offset);
            }
            WorkRecord::Step { item, attempt, .. } => {
                self.agent_holder(item, *attempt, rec)?;
                self.path_mut(item, *attempt)?.steps.push(offset);
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
            WorkRecord::Refused { item, reason, .. } => {
                if rec.actor != Actor::Tracker {
                    return Err(Reject::ActorNotAllowed);
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
                item.proposed_by_agent = true;
            }
            (Actor::Agent, Some(p)) if work.is_live_child() => {
                let par = self.item(p)?;
                match &par.hold {
                    Some(h) if h.by == rec.by => {}
                    _ => return Err(Reject::NoHeldParent),
                }
                let (root, depth) = (par.root.clone(), par.depth.saturating_add(1));
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

    /// Answers come from a person or the agent holding a question item under `item`.
    fn may_answer(&self, item: &ItemId, rec: &WorkLogged) -> Result<(), Reject> {
        let it = self.item(item)?;
        match rec.actor {
            Actor::Person => Ok(()),
            Actor::Tracker => Err(Reject::ActorNotAllowed),
            Actor::Agent => {
                let holds = it
                    .children
                    .iter()
                    .filter_map(|c| self.items.get(c))
                    .any(|q| {
                        q.work == WorkKind::Question
                            && q.hold.as_ref().is_some_and(|h| h.by == rec.by)
                    });
                if holds {
                    Ok(())
                } else {
                    Err(Reject::NotHolder)
                }
            }
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
