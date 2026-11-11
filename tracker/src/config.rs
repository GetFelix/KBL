//! Who may call the tracker. Felix has no way to ask which principal a token
//! belongs to, so the tracker reads a token map from a file. Each agent's token
//! maps to its name, and the name is what goes on its records.

use std::collections::HashMap;
use std::path::Path;

use agents_core::names::{AgentName, Key};
use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
struct File {
    /// Token to agent name.
    #[serde(default)]
    agents: HashMap<String, AgentName>,
    /// Token to a person's key. A person may append the records a person may write.
    #[serde(default)]
    people: HashMap<String, Key>,
}

pub(crate) enum Caller {
    Agent(AgentName),
    Person(Key),
}

#[derive(Default)]
pub(crate) struct Tokens {
    agents: HashMap<String, AgentName>,
    people: HashMap<String, Key>,
}

impl Tokens {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read the token file {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parse the token file {}", path.display()))
    }

    pub(crate) fn parse(text: &str) -> Result<Self> {
        let file: File = serde_json::from_str(text)?;
        Ok(Self {
            agents: file.agents,
            people: file.people,
        })
    }

    pub(crate) fn who(&self, token: &str) -> Option<Caller> {
        if let Some(agent) = self.agents.get(token) {
            return Some(Caller::Agent(agent.clone()));
        }
        self.people.get(token).cloned().map(Caller::Person)
    }
}

#[cfg(test)]
mod tests;
