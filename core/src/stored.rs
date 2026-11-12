//! The values kept in the caches. The board holds [`Content`](crate::content::Content)
//! under each key; the rest are here.

use serde::{Deserialize, Serialize};

use crate::artifact::ArtifactId;
use crate::names::{AgentName, TaskId};

/// A lease's value is the holder's name. The cache version at which it was
/// taken or renewed is the fencing token.
pub fn lease_value(holder: &AgentName) -> Vec<u8> {
    holder.as_str().as_bytes().to_vec()
}

/// The holder named by a lease's value.
pub fn lease_holder(value: &[u8]) -> Option<AgentName> {
    AgentName::new(std::str::from_utf8(value).ok()?).ok()
}

/// What `kbl-run` writes under an agent's key every 5 seconds, with a 15 second TTL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub role: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool: Option<String>,
}

/// The first result written for a task. A later write to the same key is a duplicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResult {
    pub attempt: u32,
    pub agent: AgentName,
    pub artifact: ArtifactId,
}
