//! One record per coordination step, in the run log's single order.
//!
//! A record holds what the writer knows. The broker adds the offset, its own
//! append time and the principal that published it, and the fold attributes
//! every record to that principal, never to a name inside the payload.
//!
//! Money is in millionths of a dollar and times are milliseconds since the
//! Unix epoch on the writer's clock, so no record holds a float.

use serde::{Deserialize, Serialize};

use crate::artifact::ArtifactId;
use crate::content::Content;
use crate::names::{AgentName, Key, TaskId};

/// An agent and its role, as listed when a run starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpec {
    pub name: AgentName,
    pub role: String,
}

/// What a record says happened. The `kind` field is the variant name in snake case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    /// Written by `agents-tool`.
    RunStarted {
        goal: String,
        budget_micros: u64,
        agents: Vec<AgentSpec>,
    },
    /// Written by `agents-run`.
    RunEnded {
        outcome: String,
    },
    /// Written by `agents-run`; `presence` is the version of the agent's presence key.
    AgentUp {
        agent: AgentName,
        role: String,
        model: String,
        presence: u64,
    },
    /// Written by the planner's `felix-mcp` when a presence key expires or is
    /// deleted; `presence` is the version that expired.
    AgentGone {
        agent: AgentName,
        presence: u64,
    },
    /// `offset` is where the task landed on the tasks stream.
    TaskPosted {
        task: TaskId,
        parent: TaskId,
        title: String,
        offset: u64,
    },
    TaskClaimed {
        task: TaskId,
        attempt: u32,
        deadline_ms: u64,
    },
    TaskExtended {
        task: TaskId,
        attempt: u32,
        deadline_ms: u64,
    },
    TaskReleased {
        task: TaskId,
        attempt: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The result won the results key.
    TaskDone {
        task: TaskId,
        attempt: u32,
        result: ArtifactId,
    },
    /// The result lost: another copy of the task finished first. The result is kept as an artifact.
    TaskDuplicate {
        task: TaskId,
        attempt: u32,
        result: ArtifactId,
    },
    /// Attempts used up, or given up.
    TaskDead {
        task: TaskId,
        attempt: u32,
        reason: String,
    },
    /// `version` is the cache version the put wrote.
    BoardPut {
        key: Key,
        version: u64,
        value: Content,
    },
    /// A conditional put was refused; `found` is the version, and `value` the content, now there.
    BoardConflict {
        key: Key,
        found: u64,
        value: Content,
    },
    LeaseTaken {
        key: Key,
        version: u64,
        ttl_ms: u64,
    },
    LeaseRenewed {
        key: Key,
        version: u64,
        ttl_ms: u64,
    },
    LeaseReleased {
        key: Key,
        version: u64,
    },
    /// The lease expired, or was taken by someone else, while `holder` still thought it had it.
    LeaseLost {
        key: Key,
        version: u64,
        holder: AgentName,
    },
    /// `offset` is where the message landed on the messages stream.
    Message {
        to: AgentName,
        corr: String,
        offset: u64,
    },
    /// Written by `agents-run` before a model call; `reserve_micros` is the worst case taken from the budget.
    ModelStarted {
        agent: AgentName,
        task: TaskId,
        reserve_micros: u64,
    },
    ModelCall {
        agent: AgentName,
        task: TaskId,
        prompt: ArtifactId,
        response: ArtifactId,
        tokens: u64,
        cost_micros: u64,
    },
    /// An operation Felix refused, with Felix's error.
    Refused {
        op: String,
        resource: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        error: String,
    },
}

impl Record {
    /// The `kind` field this record is written with.
    pub fn kind(&self) -> &'static str {
        match self {
            Record::RunStarted { .. } => "run_started",
            Record::RunEnded { .. } => "run_ended",
            Record::AgentUp { .. } => "agent_up",
            Record::AgentGone { .. } => "agent_gone",
            Record::TaskPosted { .. } => "task_posted",
            Record::TaskClaimed { .. } => "task_claimed",
            Record::TaskExtended { .. } => "task_extended",
            Record::TaskReleased { .. } => "task_released",
            Record::TaskDone { .. } => "task_done",
            Record::TaskDuplicate { .. } => "task_duplicate",
            Record::TaskDead { .. } => "task_dead",
            Record::BoardPut { .. } => "board_put",
            Record::BoardConflict { .. } => "board_conflict",
            Record::LeaseTaken { .. } => "lease_taken",
            Record::LeaseRenewed { .. } => "lease_renewed",
            Record::LeaseReleased { .. } => "lease_released",
            Record::LeaseLost { .. } => "lease_lost",
            Record::Message { .. } => "message",
            Record::ModelStarted { .. } => "model_started",
            Record::ModelCall { .. } => "model_call",
            Record::Refused { .. } => "refused",
        }
    }
}
