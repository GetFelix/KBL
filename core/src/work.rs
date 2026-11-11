//! What `work.events` holds, and the keys the tracker keeps beside it.
//!
//! The stream is the record: an item's state is a fold of it ([`crate::work_fold`]).
//! The keys are written by the tracker with `cache_put_if`, so a person and an
//! agent editing one item cannot overwrite each other. No record here is
//! written by a model call, and the fold makes none.

use serde::{Deserialize, Serialize};

use crate::content::Content;
use crate::names::{InvalidName, ItemId, Key};

/// The one stream every change to work goes on.
pub const WORK_EVENTS: &str = "work.events";

/// The ready stream for a tier, which `work_next` polls from `p0` up.
pub fn ready_stream(tier: u8) -> String {
    format!("work.ready.p{tier}")
}

/// The cache key of an item's fields.
pub fn item_key(item: &ItemId) -> Result<Key, InvalidName> {
    Key::new(format!("i-{item}"))
}

/// The cache key of the edge "`from` waits on `to`".
pub fn edge_key(from: &ItemId, to: &ItemId) -> Result<Key, InvalidName> {
    Key::new(format!("e-{from}-{to}"))
}

/// The cache key of one context entry on an item.
pub fn context_key(item: &ItemId, entry: &Key) -> Result<Key, InvalidName> {
    Key::new(format!("c-{item}-{entry}"))
}

/// The cache key of a root's work budget. Compare-and-set on it enforces the
/// budget, since Felix has no bounded add.
pub fn budget_key(root: &ItemId) -> Result<Key, InvalidName> {
    Key::new(format!("b-{root}"))
}

/// The cache key that makes a child's creation idempotent: the parent plus the
/// agent's own key, written only if absent.
pub fn child_key(parent: &ItemId, key: &Key) -> Result<Key, InvalidName> {
    Key::new(format!("k-{parent}-{key}"))
}

/// The cache key that dedupes an upstream bug, written only if absent.
pub fn upstream_key(source: &str, id: &str) -> Result<Key, InvalidName> {
    Key::new(format!("u-{source}-{id}"))
}

/// Who a record is attributed to. The tracker sets it from the token that
/// made the call, never from the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Agent,
    Person,
    Tracker,
}

/// What an item asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    Fix,
    Triage,
    Augment,
    Split,
    Question,
    Test,
}

impl WorkKind {
    /// Kinds an agent may create live under an item it holds.
    pub fn is_live_child(self) -> bool {
        matches!(
            self,
            WorkKind::Split | WorkKind::Augment | WorkKind::Question | WorkKind::Test
        )
    }
}

/// The part of a fix a decision belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Understand,
    Reproduce,
    Cause,
    Fix,
    Verify,
}

/// A step a decision rests on, by stream and offset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    pub stream: String,
    pub offset: u64,
}

/// What a person added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanKind {
    Note,
    Answer,
    ReviewComment,
    PriorityChange,
}

/// Why the tracker refused an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// Over the root's work budget. The root goes to Needs you.
    Budget,
    /// Over a per-attempt or per-agent cap.
    Limit,
    Invalid,
}

/// What a record says happened. `kind` is the variant name in snake case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkRecord {
    /// A root pulled from an upstream tracker. `(source, upstream_id)` is imported once.
    Imported {
        item: ItemId,
        source: String,
        upstream_id: String,
        title: String,
    },
    /// A new item. With a live kind, an agent needs to hold `parent`; an agent's
    /// `fix` is a proposal that a person accepts. `key` makes a retry a no-op.
    Proposed {
        item: ItemId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<ItemId>,
        key: Key,
        work: WorkKind,
        title: String,
    },
    /// The answer to an item's triage work: a proposal for a person.
    Triaged {
        item: ItemId,
        tier: u8,
        why: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duplicate_of: Option<ItemId>,
    },
    /// A person (or the tracker for them) accepts the item into the queue.
    Accepted {
        item: ItemId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tier: Option<u8>,
    },
    /// `item` waits on `to`.
    Linked { item: ItemId, to: ItemId },
    /// A context entry; the same `entry` again replaces it.
    Context {
        item: ItemId,
        entry: Key,
        text: Content,
    },
    Claimed {
        item: ItemId,
        attempt: u32,
        deadline_ms: u64,
    },
    Progress {
        item: ItemId,
        attempt: u32,
        note: Content,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pr: Option<String>,
    },
    /// `cost_micros` is what the agent reports and the tracker cannot verify.
    Done {
        item: ItemId,
        attempt: u32,
        #[serde(default)]
        cost_micros: u64,
    },
    Released {
        item: ItemId,
        attempt: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    GaveUp {
        item: ItemId,
        attempt: u32,
        reason: String,
        #[serde(default)]
        cost_micros: u64,
    },
    /// The holder asks a person; the item waits in Needs you.
    Question {
        item: ItemId,
        attempt: u32,
        text: Content,
    },
    /// `question` is the offset of the question it answers, which may be a
    /// `question` record or the offset a `question` work item was created at.
    Answer {
        item: ItemId,
        question: u64,
        text: Content,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        evidence: Vec<Evidence>,
    },
    Decision {
        item: ItemId,
        attempt: u32,
        phase: Phase,
        so_far: String,
        question: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
        chosen: String,
        why: String,
        commit: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        evidence: Vec<Evidence>,
    },
    Step {
        item: ItemId,
        attempt: u32,
        command: String,
        exit_code: i32,
        output: Content,
    },
    /// `decision` is the offset of the decision this edit carries out.
    Edit {
        item: ItemId,
        attempt: u32,
        file: String,
        hunk: Content,
        decision: u64,
    },
    /// `attempt` names the path it steers; none means the item as a whole.
    HumanInput {
        item: ItemId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attempt: Option<u32>,
        input: HumanKind,
        text: Content,
    },
    /// The next attempt starts from the decision at `at` on `from_attempt`.
    /// `note` is the offset of the `human_input` that steers it.
    ForkedFrom {
        item: ItemId,
        from_attempt: u32,
        at: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<u64>,
    },
    /// Sets a root's budget outright. Only a person may; this is how it is raised.
    BudgetSet {
        root: ItemId,
        items_left: u32,
        max_depth: u8,
        cost_left_micros: u64,
        redos_left: u8,
    },
    /// An operation the tracker refused and logged.
    Refused {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<ItemId>,
        op: String,
        reason: RefusalReason,
    },
    /// A kind this version does not know. A fold skips it.
    #[serde(other)]
    Unknown,
}

impl WorkRecord {
    /// The `kind` field this record is written with.
    pub fn kind(&self) -> &'static str {
        match self {
            WorkRecord::Imported { .. } => "imported",
            WorkRecord::Proposed { .. } => "proposed",
            WorkRecord::Triaged { .. } => "triaged",
            WorkRecord::Accepted { .. } => "accepted",
            WorkRecord::Linked { .. } => "linked",
            WorkRecord::Context { .. } => "context",
            WorkRecord::Claimed { .. } => "claimed",
            WorkRecord::Progress { .. } => "progress",
            WorkRecord::Done { .. } => "done",
            WorkRecord::Released { .. } => "released",
            WorkRecord::GaveUp { .. } => "gave_up",
            WorkRecord::Question { .. } => "question",
            WorkRecord::Answer { .. } => "answer",
            WorkRecord::Decision { .. } => "decision",
            WorkRecord::Step { .. } => "step",
            WorkRecord::Edit { .. } => "edit",
            WorkRecord::HumanInput { .. } => "human_input",
            WorkRecord::ForkedFrom { .. } => "forked_from",
            WorkRecord::BudgetSet { .. } => "budget_set",
            WorkRecord::Refused { .. } => "refused",
            WorkRecord::Unknown => "unknown",
        }
    }
}

/// A record as it sits on `work.events`: when, which principal, and in what role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkLogged {
    pub at_ms: u64,
    pub by: Key,
    pub actor: Actor,
    #[serde(flatten)]
    pub record: WorkRecord,
}
