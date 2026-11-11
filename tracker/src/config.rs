//! Who may call the tracker. Felix has no way to ask which principal a token
//! belongs to, so the tracker reads a token map from a file. Each agent's token
//! maps to its name, and the name is what goes on its records.

use std::collections::BTreeMap;
use std::path::Path;

use agents_core::names::{AgentName, Key};
use anyhow::{Context, Result};
use serde::Deserialize;

/// A JSON object read as pairs, so a token listed twice is seen instead of overwritten.
struct Pairs<T>(Vec<(String, T)>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Pairs<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for V<T> {
            type Value = Pairs<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut m: A,
            ) -> Result<Pairs<T>, A::Error> {
                let mut pairs = Vec::new();
                while let Some(pair) = m.next_entry()? {
                    pairs.push(pair);
                }
                Ok(Pairs(pairs))
            }
        }
        d.deserialize_map(V(std::marker::PhantomData))
    }
}

#[derive(Deserialize)]
struct File {
    /// Token to agent name.
    #[serde(default = "Pairs::empty")]
    agents: Pairs<AgentName>,
    /// Token to a person's key. A person may append the records a person may write.
    #[serde(default = "Pairs::empty")]
    people: Pairs<Key>,
    /// Token to an ingest worker's key. It may append the upstream sync records and keep its cursor.
    #[serde(default = "Pairs::empty")]
    ingest: Pairs<Key>,
    /// Agent name to the repos it may reach outside its own tree.
    #[serde(default)]
    grants: BTreeMap<String, Vec<String>>,
}

/// Agent name to the repos it may file prerequisites in and link to, beyond
/// the repo of the tree it is working in.
pub(crate) type Grants = BTreeMap<String, Vec<String>>;

impl<T> Pairs<T> {
    fn empty() -> Self {
        Pairs(Vec::new())
    }
}

pub(crate) enum Caller {
    Agent(AgentName),
    Person(Key),
    Ingest(Key),
}

#[derive(Default)]
pub(crate) struct Tokens {
    agents: Vec<(String, AgentName)>,
    people: Vec<(String, Key)>,
    ingest: Vec<(String, Key)>,
    grants: Grants,
}

impl Tokens {
    /// Reads the token file. It holds credentials, so it must be readable by its
    /// owner only.
    pub(crate) fn load(path: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path)
                .with_context(|| format!("read the token file {}", path.display()))?
                .permissions()
                .mode()
                & 0o777;
            anyhow::ensure!(
                mode == 0o600 || mode == 0o400,
                "the token file {} has mode {mode:o}; set it to 600 or 400",
                path.display()
            );
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read the token file {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parse the token file {}", path.display()))
    }

    pub(crate) fn parse(text: &str) -> Result<Self> {
        let file: File = serde_json::from_str(text)?;
        let mut seen = std::collections::HashSet::new();
        let all = file
            .agents
            .0
            .iter()
            .map(|(t, _)| t)
            .chain(file.people.0.iter().map(|(t, _)| t))
            .chain(file.ingest.0.iter().map(|(t, _)| t));
        for token in all {
            anyhow::ensure!(!token.is_empty(), "a token is empty");
            anyhow::ensure!(
                seen.insert(token.as_str()),
                "a token is listed more than once, or as both an agent and a person"
            );
        }
        for (agent, repos) in &file.grants {
            anyhow::ensure!(
                file.agents.0.iter().any(|(_, a)| a.as_str() == agent),
                "grants name {agent:?}, which is not an agent"
            );
            anyhow::ensure!(
                repos.iter().all(|r| r.contains('/')),
                "a grant for {agent:?} is not an owner/repo"
            );
        }
        Ok(Self {
            agents: file.agents.0,
            people: file.people.0,
            ingest: file.ingest.0,
            grants: file.grants,
        })
    }

    pub(crate) fn grants(&self) -> Grants {
        self.grants.clone()
    }

    /// Looks the token up without stopping at the first match or at the first
    /// differing byte, so response time does not tell how much of a token is right.
    pub(crate) fn who(&self, token: &str) -> Option<Caller> {
        let mut found = None;
        for (t, agent) in &self.agents {
            if same(t, token) {
                found = Some(Caller::Agent(agent.clone()));
            }
        }
        for (t, person) in &self.people {
            if same(t, token) {
                found = Some(Caller::Person(person.clone()));
            }
        }
        for (t, worker) in &self.ingest {
            if same(t, token) {
                found = Some(Caller::Ingest(worker.clone()));
            }
        }
        found
    }
}

/// Equal in time that depends on the lengths only.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

#[cfg(test)]
mod tests;
