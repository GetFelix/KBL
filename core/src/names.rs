//! Names. Felix puts run ids and keys inside stream, cache and role names, so
//! they use the alphabet felix-gateway allows for keys: 1 to 64 characters of
//! `[A-Za-z0-9_-]`. A name that passes can be read by every tool, the viewer
//! included.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Longest key, run id or agent name.
pub const MAX_NAME_LEN: usize = 64;

/// Whether `s` is a valid key under felix-gateway's rule.
pub fn is_valid_key(s: &str) -> bool {
    (1..=MAX_NAME_LEN).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A string that failed the key rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidName(pub String);

impl fmt::Display for InvalidName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} is not a valid name: use 1 to {MAX_NAME_LEN} characters of A-Z, a-z, 0-9, - and _",
            self.0
        )
    }
}

impl std::error::Error for InvalidName {}

macro_rules! name_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Result<Self, InvalidName> {
                let s = s.into();
                if is_valid_key(&s) { Ok(Self(s)) } else { Err(InvalidName(s)) }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = InvalidName;
            fn from_str(s: &str) -> Result<Self, InvalidName> {
                Self::new(s)
            }
        }

        // Decoding checks the rule, so a record with a bad name never gets past the edge.
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::new(s).map_err(serde::de::Error::custom)
            }
        }
    };
}

name_type! {
    /// The id of a run, part of every resource name.
    RunId
}
name_type! {
    /// A task id.
    TaskId
}
name_type! {
    /// A work item's id on `work.events`.
    ItemId
}
name_type! {
    /// A key in the board or the leases cache. The board's grants go by
    /// prefix (`plan-*`, `w3-*`, `shared-*`), so the prefix is part of the name.
    Key
}

/// An agent's name: `planner`, `w1` to `w8`, `critic`. No `-`, so a grant on
/// the prefix `w1-` never covers another agent's keys, and not `plan` or
/// `shared`, which are board prefixes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct AgentName(String);

impl AgentName {
    pub fn new(s: impl Into<String>) -> Result<Self, InvalidName> {
        let s = s.into();
        if is_valid_key(&s) && !s.contains('-') && !matches!(s.as_str(), "plan" | "shared") {
            Ok(Self(s))
        } else {
            Err(InvalidName(s))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for AgentName {
    type Err = InvalidName;
    fn from_str(s: &str) -> Result<Self, InvalidName> {
        Self::new(s)
    }
}

impl<'de> Deserialize<'de> for AgentName {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

impl Key {
    /// Whether the key starts with `prefix`, as a grant on `prefix*` does.
    pub fn has_prefix(&self, prefix: &str) -> bool {
        self.0.starts_with(prefix)
    }
}

/// The nine Felix resources of one run. Runs share a namespace, so the run id
/// is in each name and grants name a run's resources only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resources {
    run: RunId,
}

impl Resources {
    pub fn new(run: RunId) -> Self {
        Self { run }
    }

    pub fn run(&self) -> &RunId {
        &self.run
    }

    fn named(&self, kind: &str) -> String {
        format!("agents.{kind}.{}", self.run)
    }

    /// Durable stream, one shard, publishers recorded: the record of the run.
    pub fn log(&self) -> String {
        self.named("log")
    }

    /// Durable stream, one shard, consumer group [`WORKERS_GROUP`].
    pub fn tasks(&self) -> String {
        self.named("tasks")
    }

    /// Durable stream, one shard, publishers recorded; the routing key is the recipient.
    pub fn msgs(&self) -> String {
        self.named("msgs")
    }

    /// Log-backed cache of shared findings.
    pub fn board(&self) -> String {
        self.named("board")
    }

    /// Log-backed cache with a TTL per put, one key per lockable thing.
    pub fn leases(&self) -> String {
        self.named("leases")
    }

    /// Log-backed cache with a TTL per put, one key per agent.
    pub fn alive(&self) -> String {
        self.named("alive")
    }

    /// Log-backed cache, one key per task, first write wins.
    pub fn results(&self) -> String {
        self.named("results")
    }

    /// Log-backed cache of prompts, model output and tool output, by content hash.
    pub fn blobs(&self) -> String {
        self.named("blobs")
    }

    /// Counters for tokens and cost.
    pub fn spend(&self) -> String {
        self.named("spend")
    }

    /// Every resource name, for creating and deleting a run's resources.
    pub fn all(&self) -> [String; 9] {
        [
            self.log(),
            self.tasks(),
            self.msgs(),
            self.board(),
            self.leases(),
            self.alive(),
            self.results(),
            self.blobs(),
            self.spend(),
        ]
    }
}

/// The consumer group workers poll the tasks stream as.
pub const WORKERS_GROUP: &str = "workers";

/// Keys in the spend counters.
pub mod spend {
    use super::{AgentName, Key};

    /// The run's total.
    pub const RUN: &str = "run";

    /// An agent's own total.
    pub fn agent(agent: &AgentName) -> Result<Key, super::InvalidName> {
        Key::new(format!("agent-{agent}"))
    }

    /// An agent's rate-limit window, `rl-<agent>-<minute>`. Counters have no
    /// TTL, so `agents-tool export` deletes these at the end of a run.
    pub fn rate_window(agent: &AgentName, minute: u64) -> Result<Key, super::InvalidName> {
        Key::new(format!("rl-{agent}-{minute}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_rule() {
        assert!(is_valid_key("w3-limiter_clock"));
        assert!(is_valid_key(&"a".repeat(64)));
        assert!(!is_valid_key(""));
        assert!(!is_valid_key(&"a".repeat(65)));
        for bad in ["a.b", "a b", "a/b", "a*", "é", "a:b"] {
            assert!(!is_valid_key(bad), "{bad}");
        }
    }

    #[test]
    fn resource_names_carry_the_run() {
        let r = Resources::new(RunId::new("r-7f3a").unwrap());
        assert_eq!(r.log(), "agents.log.r-7f3a");
        assert_eq!(r.tasks(), "agents.tasks.r-7f3a");
        assert_eq!(r.msgs(), "agents.msgs.r-7f3a");
        assert_eq!(r.board(), "agents.board.r-7f3a");
        assert_eq!(r.leases(), "agents.leases.r-7f3a");
        assert_eq!(r.alive(), "agents.alive.r-7f3a");
        assert_eq!(r.results(), "agents.results.r-7f3a");
        assert_eq!(r.blobs(), "agents.blobs.r-7f3a");
        assert_eq!(r.spend(), "agents.spend.r-7f3a");
        let all = r.all();
        let mut sorted = all.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 9);
    }

    #[test]
    fn names_are_checked_when_decoded() {
        assert!(serde_json::from_str::<RunId>("\"r-1\"").is_ok());
        assert!(serde_json::from_str::<RunId>("\"r.1\"").is_err());
        assert!(serde_json::from_str::<Key>("\"\"").is_err());
    }

    #[test]
    fn agent_names_cannot_borrow_a_prefix() {
        for ok in ["planner", "w1", "w8", "critic"] {
            assert!(AgentName::new(ok).is_ok(), "{ok}");
        }
        for bad in ["w1-b", "plan", "shared", "", "a.b"] {
            assert!(AgentName::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn prefixes() {
        let k = Key::new("plan-review").unwrap();
        assert!(k.has_prefix("plan-"));
        assert!(!k.has_prefix("w3-"));
    }

    #[test]
    fn spend_keys_fit() {
        let long = AgentName::new("a".repeat(64)).unwrap();
        assert_eq!(
            spend::agent(&AgentName::new("w3").unwrap())
                .unwrap()
                .as_str(),
            "agent-w3"
        );
        assert!(spend::agent(&long).is_err());
        assert_eq!(
            spend::rate_window(&AgentName::new("w3").unwrap(), 29_000_000)
                .unwrap()
                .as_str(),
            "rl-w3-29000000"
        );
        assert!(spend::rate_window(&long, 1).is_err());
    }
}
