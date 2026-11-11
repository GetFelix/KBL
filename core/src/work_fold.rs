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
use crate::work::{Actor, Evidence, RefusalReason, WORK_EVENTS, WorkKind, WorkLogged, WorkRecord};

#[cfg(test)]
mod tests;

const DEFAULT_MAX_ITEMS: u32 = 12;
const DEFAULT_MAX_DEPTH: u8 = 3;
const DEFAULT_COST_MICROS: u64 = 10_000_000;
const DEFAULT_REDOS: u8 = 3;
const DEFAULT_TIER: u8 = 2;

/// A root's work budget. Agent records only ever lower it; a person's
/// `budget_set` is the one way up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub items_left: u32,
    pub max_depth: u8,
    pub cost_left_micros: u64,
    pub redos_left: u8,
}

impl Budget {
    fn for_new_root() -> Self {
        Budget {
            // The root takes one of the tree's slots.
            items_left: DEFAULT_MAX_ITEMS - 1,
            max_depth: DEFAULT_MAX_DEPTH,
            cost_left_micros: DEFAULT_COST_MICROS,
            redos_left: DEFAULT_REDOS,
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
    Done,
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
    pub attempts: u32,
    pub done: bool,
    pub gave_up: bool,
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
    child_keys: BTreeSet<(ItemId, Key)>,
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
        Some(if it.done {
            Stage::Done
        } else if it.open_question.is_some() || it.gave_up || it.over_budget {
            Stage::NeedsYou
        } else if it.hold.is_some() {
            Stage::Held
        } else if !it.accepted {
            if it.proposed_by_agent {
                Stage::Proposed
            } else {
                Stage::Triage
            }
        } else if self.edges.iter().any(|(a, b)| a == id && !self.is_done(b)) {
            Stage::Blocked
        } else {
            Stage::Ready
        })
    }

    /// Children of `id` that are not done.
    pub fn open_children(&self, id: &ItemId) -> Vec<&ItemId> {
        self.items.get(id).map_or_else(Vec::new, |it| {
            it.children.iter().filter(|c| !self.is_done(c)).collect()
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
        out.sort_unstable();
        out
    }

    fn is_done(&self, id: &ItemId) -> bool {
        self.items.get(id).is_some_and(|i| i.done)
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
            attempts: 0,
            done: false,
            gave_up: false,
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
            }
            WorkRecord::Linked { item, to } => {
                self.item(item)?;
                self.item(to)?;
                if item == to || self.reaches(to, item) {
                    return Err(Reject::Cycle);
                }
                self.edges.insert((item.clone(), to.clone()));
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
                if self.stage(item).ok_or(Reject::UnknownItem)? != Stage::Ready {
                    return Err(Reject::NotReady);
                }
                let it = self.item_mut(item)?;
                if *attempt != it.attempts + 1 {
                    return Err(Reject::WrongAttempt);
                }
                it.attempts = *attempt;
                it.hold = Some(Hold {
                    by: rec.by.clone(),
                    attempt: *attempt,
                    deadline_ms: *deadline_ms,
                });
                it.paths.push(Path {
                    attempt: *attempt,
                    forked_from: it.pending_fork.take(),
                    decisions: Vec::new(),
                    steps: Vec::new(),
                    edits: Vec::new(),
                    human_inputs: Vec::new(),
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
                if !self.open_children(item).is_empty() {
                    return Err(Reject::OpenChildren);
                }
                let root = it.root.clone();
                let it = self.item_mut(item)?;
                it.done = true;
                it.hold = None;
                it.cost_micros += cost_micros;
                self.draw_cost(&root, *cost_micros);
            }
            WorkRecord::Released { item, attempt, .. } => {
                if self.acting_on(item, *attempt, rec)?.hold.is_none() {
                    return Err(Reject::NotHeld);
                }
                self.item_mut(item)?.hold = None;
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
                it.gave_up = true;
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
                if it.hold.is_some() || it.done || it.pending_fork.is_some() {
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
                let root = it.root.clone();
                let b = self.budgets.get_mut(&root).ok_or(Reject::UnknownItem)?;
                if b.redos_left == 0 {
                    return Err(Reject::NoRedosLeft);
                }
                b.redos_left -= 1;
                let it = self.item_mut(item)?;
                it.pending_fork = Some(Fork {
                    from_attempt: *from_attempt,
                    at: *at,
                    note: *note,
                });
                it.gave_up = false;
            }
            WorkRecord::BudgetSet {
                root,
                items_left,
                max_depth,
                cost_left_micros,
                redos_left,
            } => {
                if rec.actor != Actor::Person {
                    return Err(Reject::ActorNotAllowed);
                }
                let b = self.budgets.get_mut(root).ok_or(Reject::UnknownItem)?;
                *b = Budget {
                    items_left: *items_left,
                    max_depth: *max_depth,
                    cost_left_micros: *cost_left_micros,
                    redos_left: *redos_left,
                };
                self.item_mut(root)?.over_budget = false;
            }
            WorkRecord::Refused { item, reason, .. } => {
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
        let mut item = Self::blank(id, work, title);
        match (rec.actor, parent) {
            // An agent's fix is out of scope for its task: a root once a person accepts it.
            (Actor::Agent, _) if work == WorkKind::Fix => {
                item.proposed_by_agent = true;
                self.budgets.insert(id.clone(), Budget::for_new_root());
            }
            (Actor::Agent, Some(p)) if work.is_live_child() => {
                let par = self.item(p)?;
                match &par.hold {
                    Some(h) if h.by == rec.by => {}
                    _ => return Err(Reject::NoHeldParent),
                }
                if self.child_keys.contains(&(p.clone(), key.clone())) {
                    return Err(Reject::Duplicate);
                }
                let (root, depth) = (par.root.clone(), par.depth);
                let b = self.budgets.get_mut(&root).ok_or(Reject::UnknownItem)?;
                if depth + 1 > b.max_depth {
                    return Err(Reject::TooDeep);
                }
                if b.items_left == 0 || b.cost_left_micros == 0 {
                    return Err(Reject::OverBudget);
                }
                b.items_left -= 1;
                item.depth = depth + 1;
                item.parent = Some(p.clone());
                item.root = root;
                item.accepted = true;
            }
            (Actor::Agent, _) => return Err(Reject::NoHeldParent),
            (_, Some(p)) => {
                // Items people and the tracker add draw nothing from the root budget.
                let par = self.item(p)?;
                if self.child_keys.contains(&(p.clone(), key.clone())) {
                    return Err(Reject::Duplicate);
                }
                item.depth = par.depth.saturating_add(1);
                item.root = par.root.clone();
                item.parent = Some(p.clone());
                item.accepted = true;
            }
            (actor, None) => {
                self.budgets.insert(id.clone(), Budget::for_new_root());
                item.accepted = actor == Actor::Person;
            }
        }
        if let Some(p) = &item.parent {
            self.child_keys.insert((p.clone(), key.clone()));
        }
        self.new_item(item);
        Ok(())
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
