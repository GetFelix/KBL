//! The task that goes on the tasks stream.

use serde::{Deserialize, Serialize};

use crate::content::Content;
use crate::names::{Key, TaskId};

/// A unit of work the planner posts and one worker finishes.
///
/// How many times Felix has handed the task out is not here: that is the
/// attempt number in the poll result, which the claim records carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
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
