//! The task that goes on the tasks stream.
//!
//! A task has a format, as a record on `work.events` does: see docs/upgrades.md.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::codec::{Codec, DecodeError, newer_format};
use crate::content::Content;
use crate::names::{Key, TaskId};

/// The format of every task posted before formats existed. Frozen: its tasks
/// are written without a `v` field.
pub const TASK_FORMAT_V1: u32 = 1;

/// The highest task format this build reads and writes.
pub const TASK_FORMAT: u32 = TASK_FORMAT_V1;

/// A unit of work the planner posts and one worker finishes.
///
/// How many times Felix has handed the task out is not here: that is the
/// attempt number in the poll result, which the claim records carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Left out at [`TASK_FORMAT_V1`]. Decoding fails above [`TASK_FORMAT`]:
    /// use [`read`] to tell that from a payload that is not a task.
    #[serde(
        default = "format_v1",
        skip_serializing_if = "is_format_v1",
        deserialize_with = "readable_format"
    )]
    pub v: u32,
    pub id: TaskId,
    /// The task or plan it came from.
    pub parent: TaskId,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Content>,
    /// A lease the worker takes before starting, when the task touches something shared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<Key>,
    /// Milliseconds since the Unix epoch after which the task is not worth finishing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
}

impl Task {
    /// A task with no body, lease or deadline, at the first format. A field
    /// added later that a worker must honour raises [`Task::format`].
    pub fn new(id: TaskId, parent: TaskId, title: String) -> Self {
        Self {
            v: TASK_FORMAT_V1,
            id,
            parent,
            title,
            body: None,
            lease: None,
            deadline_ms: None,
        }
    }

    /// The task with `v` set to [`Task::format`], ready to post.
    pub fn stamped(mut self) -> Self {
        self.v = self.format();
        self
    }

    /// The lowest format whose workers run this task as this build does.
    pub fn format(&self) -> u32 {
        // No `..`: a new field does not compile until this says what it needs.
        let Task {
            v: _,
            id: _,
            parent: _,
            title: _,
            body: _,
            lease: _,
            deadline_ms: _,
        } = self;
        TASK_FORMAT_V1
    }
}

/// Why a payload on the tasks stream was not read.
#[derive(Debug)]
pub enum TaskReadError {
    /// The task is of a format above [`TASK_FORMAT`]. This worker must not
    /// run it, and must leave it for a worker that can.
    Newer {
        v: u32,
    },
    Malformed(DecodeError),
}

impl fmt::Display for TaskReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskReadError::Newer { v } => write!(
                f,
                "the task has format {v} and this build reads up to {TASK_FORMAT}: upgrade this worker"
            ),
            TaskReadError::Malformed(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for TaskReadError {}

/// Decodes a payload of the tasks stream, and says when it failed because the
/// task is newer than this build.
pub fn read(payload: &[u8]) -> Result<Task, TaskReadError> {
    Task::decode(payload).map_err(|e| match newer_format(payload, TASK_FORMAT) {
        Some(v) => TaskReadError::Newer { v },
        None => TaskReadError::Malformed(e),
    })
}

fn format_v1() -> u32 {
    TASK_FORMAT_V1
}

fn is_format_v1(v: &u32) -> bool {
    *v == TASK_FORMAT_V1
}

fn readable_format<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let v = u32::deserialize(d)?;
    if (TASK_FORMAT_V1..=TASK_FORMAT).contains(&v) {
        Ok(v)
    } else {
        Err(serde::de::Error::custom(format_args!(
            "task format {v} is not one this build reads ({TASK_FORMAT_V1} to {TASK_FORMAT})"
        )))
    }
}
