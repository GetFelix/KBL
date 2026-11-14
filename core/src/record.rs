//! One record per coordination step, in the run log's single order.
//!
//! A record holds what the writer knows. The broker adds the offset, its own
//! append time and the principal that published it, and the fold attributes
//! every record to that principal, never to a name inside the payload.
//!
//! Money is in millionths of a dollar and times are milliseconds since the
//! Unix epoch on the writer's clock, so no record holds a float.
//!
//! Each record has a format, as on `work.events`: see docs/upgrades.md.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::artifact::ArtifactId;
use crate::codec::{Codec, DecodeError, newer_format};
use crate::content::Content;
use crate::names::{AgentName, Key, TaskId};

/// The format of every run log record written before formats existed. Frozen:
/// its records are written without a `v` field.
pub const RUN_LOG_FORMAT_V1: u32 = 1;

/// The highest run log format this build reads and writes.
pub const RUN_LOG_FORMAT: u32 = RUN_LOG_FORMAT_V1;

/// An agent and its role, as listed when a run starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpec {
    pub name: AgentName,
    pub role: String,
}

/// What `kbl-mcp` and `kbl-run` write to the run log: a record, when the
/// writer wrote it and who it says wrote it. Until felix-gateway passes the
/// broker's append time and recorded publisher on (gaps 9 and 7), these two
/// fields are all a viewer has, and it marks them unverified; the fold prefers
/// the broker's values when it has them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Logged {
    /// Left out at [`RUN_LOG_FORMAT_V1`]. Decoding fails above
    /// [`RUN_LOG_FORMAT`]: use [`read`] to tell that from a malformed record.
    #[serde(
        default = "format_v1",
        skip_serializing_if = "is_format_v1",
        deserialize_with = "readable_format"
    )]
    pub v: u32,
    /// Milliseconds since the Unix epoch on the writer's clock.
    pub at_ms: u64,
    /// An agent's name, or `kbl-tool` or `kbl-run` for the two programs that write for the run.
    pub by: Key,
    #[serde(flatten)]
    pub record: Record,
}

impl Logged {
    /// A record to append, stamped with the lowest format that reads it.
    pub fn new(at_ms: u64, by: Key, record: Record) -> Self {
        Self {
            v: record.format(),
            at_ms,
            by,
            record,
        }
    }
}

/// Why a payload on a run log was not read.
#[derive(Debug)]
pub enum RunLogReadError {
    /// The record is of a format above [`RUN_LOG_FORMAT`]. A reader that
    /// builds state from the log must stop here: skipping it would show a run
    /// its writers did not record.
    Newer {
        v: u32,
    },
    Malformed(DecodeError),
}

impl fmt::Display for RunLogReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunLogReadError::Newer { v } => write!(
                f,
                "the run log record has format {v} and this build reads up to {RUN_LOG_FORMAT}: upgrade this reader"
            ),
            RunLogReadError::Malformed(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RunLogReadError {}

/// Decodes a payload of a run log, and says when it failed because the record
/// is newer than this build.
pub fn read(payload: &[u8]) -> Result<Logged, RunLogReadError> {
    Logged::decode(payload).map_err(|e| match newer_format(payload, RUN_LOG_FORMAT) {
        Some(v) => RunLogReadError::Newer { v },
        None => RunLogReadError::Malformed(e),
    })
}

fn format_v1() -> u32 {
    RUN_LOG_FORMAT_V1
}

fn is_format_v1(v: &u32) -> bool {
    *v == RUN_LOG_FORMAT_V1
}

fn readable_format<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let v = u32::deserialize(d)?;
    if (RUN_LOG_FORMAT_V1..=RUN_LOG_FORMAT).contains(&v) {
        Ok(v)
    } else {
        Err(serde::de::Error::custom(format_args!(
            "run log record format {v} is not one this build reads ({RUN_LOG_FORMAT_V1} to {RUN_LOG_FORMAT})"
        )))
    }
}

/// What a record says happened. The `kind` field is the variant name in snake case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Record {
    /// Written by `kbl-tool`.
    RunStarted {
        goal: String,
        budget_micros: u64,
        agents: Vec<AgentSpec>,
    },
    /// Written by `kbl-run`.
    RunEnded {
        outcome: String,
    },
    /// Written by `kbl-run`; `presence` is the version of the agent's presence key.
    AgentUp {
        agent: AgentName,
        role: String,
        model: String,
        presence: u64,
    },
    /// Written by the planner's `kbl-mcp` when a presence key expires or is
    /// deleted; `presence` is the version that expired.
    AgentGone {
        agent: AgentName,
        presence: u64,
    },
    /// `offset` is where the task landed on the tasks stream. Every later task
    /// record carries the offset of the copy it is about, so a post that was
    /// retried and landed twice is two tasks to the fold, not one.
    TaskPosted {
        task: TaskId,
        parent: TaskId,
        title: String,
        offset: u64,
    },
    TaskClaimed {
        task: TaskId,
        offset: u64,
        attempt: u32,
        deadline_ms: u64,
    },
    TaskExtended {
        task: TaskId,
        offset: u64,
        attempt: u32,
        deadline_ms: u64,
    },
    TaskReleased {
        task: TaskId,
        offset: u64,
        attempt: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The result won the results key.
    TaskDone {
        task: TaskId,
        offset: u64,
        attempt: u32,
        result: ArtifactId,
    },
    /// The result lost: another copy of the task finished first. The result is kept as an artifact.
    TaskDuplicate {
        task: TaskId,
        offset: u64,
        attempt: u32,
        result: ArtifactId,
    },
    /// Attempts used up, or given up.
    TaskDead {
        task: TaskId,
        offset: u64,
        attempt: u32,
        reason: String,
    },
    /// `version` is the cache version the put wrote and `prev` the version it
    /// was conditioned on (none for a new key). A write that never got logged
    /// shows as a `prev` the log never produced.
    BoardPut {
        key: Key,
        version: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prev: Option<u64>,
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
    /// `prev` is the version renewed from; `version` is the new fencing token.
    LeaseRenewed {
        key: Key,
        version: u64,
        prev: u64,
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
    /// `offset` is where the message landed on the messages stream. The text
    /// is repeated here so a run replays from the log alone.
    Message {
        to: AgentName,
        corr: String,
        offset: u64,
        text: Content,
    },
    /// Written by `kbl-run` before a model call; `reserve_micros` is the worst case taken from the budget.
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
    /// The reservation put the run or the agent over its limit, was added back
    /// and the agent stopped. Ends the `model_started` it follows.
    BudgetExhausted {
        agent: AgentName,
        task: TaskId,
        reserve_micros: u64,
    },
    /// A refund `kbl-run` could not confirm and did not retry, since a
    /// retried add can count twice.
    RefundUnconfirmed {
        agent: AgentName,
        micros: u64,
    },
    /// An operation that was refused, with the error: Felix's, or the
    /// worker's own when it hands back a task of a newer format.
    Refused {
        op: String,
        resource: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        error: String,
    },
    /// The worker's claim on the task lapsed, or was handed out again, while
    /// it still held the task. Its result may still land, and it may be a duplicate.
    TaskClaimLost {
        task: TaskId,
        offset: u64,
        attempt: u32,
    },
    /// A kind this version does not know. A fold skips it.
    #[serde(other)]
    Unknown,
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
            Record::BudgetExhausted { .. } => "budget_exhausted",
            Record::RefundUnconfirmed { .. } => "refund_unconfirmed",
            Record::Refused { .. } => "refused",
            Record::TaskClaimLost { .. } => "task_claim_lost",
            Record::Unknown => "unknown",
        }
    }

    /// The lowest format whose readers read this record as this build does.
    pub fn format(&self) -> u32 {
        // No wildcard: a new kind does not compile until it names its format.
        match self {
            Record::RunStarted { .. }
            | Record::RunEnded { .. }
            | Record::AgentUp { .. }
            | Record::AgentGone { .. }
            | Record::TaskPosted { .. }
            | Record::TaskClaimed { .. }
            | Record::TaskExtended { .. }
            | Record::TaskReleased { .. }
            | Record::TaskDone { .. }
            | Record::TaskDuplicate { .. }
            | Record::TaskDead { .. }
            | Record::BoardPut { .. }
            | Record::BoardConflict { .. }
            | Record::LeaseTaken { .. }
            | Record::LeaseRenewed { .. }
            | Record::LeaseReleased { .. }
            | Record::LeaseLost { .. }
            | Record::Message { .. }
            | Record::ModelStarted { .. }
            | Record::ModelCall { .. }
            | Record::BudgetExhausted { .. }
            | Record::RefundUnconfirmed { .. }
            | Record::Refused { .. }
            | Record::TaskClaimLost { .. }
            | Record::Unknown => RUN_LOG_FORMAT_V1,
        }
    }
}
