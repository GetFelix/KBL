//! What a new run is: its agents, its resources, and the `run_started` record.

use kbl_core::names::{AgentName, InvalidName, Key, Resources, RunId};
use kbl_core::record::{AgentSpec, Logged, Record};
use serde_json::{Value, json};

/// The most workers a run can ask for. The viewer's ring is built for 64.
pub(crate) const MAX_WORKERS: u32 = 64;

/// The name `run_started` is written under.
pub(crate) const WRITER: &str = "kbl-tool";

/// What the user asked for.
#[derive(Debug, Clone)]
pub(crate) struct NewRun {
    pub(crate) run: RunId,
    pub(crate) goal: String,
    pub(crate) budget_micros: u64,
    pub(crate) workers: u32,
}

impl NewRun {
    /// The planner, then `w1` to `wN`.
    pub(crate) fn agents(&self) -> Result<Vec<AgentSpec>, InvalidName> {
        let mut agents = vec![AgentSpec {
            name: AgentName::new("planner")?,
            role: "planner".into(),
        }];
        for n in 1..=self.workers {
            agents.push(AgentSpec {
                name: AgentName::new(format!("w{n}"))?,
                role: "worker".into(),
            });
        }
        Ok(agents)
    }

    pub(crate) fn started(&self, at_ms: u64) -> Result<Logged, InvalidName> {
        Ok(Logged::new(
            at_ms,
            Key::new(WRITER)?,
            Record::RunStarted {
                goal: self.goal.clone(),
                budget_micros: self.budget_micros,
                agents: self.agents()?,
            },
        ))
    }

    pub(crate) fn resources(&self) -> Resources {
        Resources::new(self.run.clone())
    }
}

/// The body of the control plane's batch create: the run's three streams and
/// six caches. The batch is all or none, so a run never half exists.
pub(crate) fn batch(resources: &Resources) -> Value {
    let streams: Vec<Value> = [resources.log(), resources.tasks(), resources.msgs()]
        .into_iter()
        .map(|name| {
            json!({
                "stream": name,
                "kind": "Stream",
                "shards": 1,
                "replication_factor": 1,
                "retention": { "max_age_seconds": null, "max_size_bytes": null },
                "consistency": "Leader",
                "delivery": "AtLeastOnce",
                "durable": true,
            })
        })
        .collect();
    let caches: Vec<Value> = [
        resources.board(),
        resources.leases(),
        resources.alive(),
        resources.results(),
        resources.blobs(),
        resources.spend(),
    ]
    .into_iter()
    .map(|name| {
        json!({
            "cache": name,
            "display_name": name,
            "shards": 1,
            "replication_factor": 1,
            "consistency": "Leader",
        })
    })
    .collect();
    json!({ "streams": streams, "caches": caches })
}

/// A dollar amount such as `5`, `0.25` or `12.345678` in millionths of a dollar.
/// Money is never a float, so a budget has exactly the cents it was given.
pub(crate) fn parse_budget(text: &str) -> Result<u64, String> {
    let bad = || format!("`{text}` is not an amount in dollars, such as 5 or 0.25");
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty()
        || (text.contains('.') && fraction.is_empty())
        || !digits(whole)
        || !digits(fraction)
        || fraction.len() > 6
    {
        return Err(bad());
    }
    let whole: u64 = whole.parse().map_err(|_| bad())?;
    let fraction: u64 = format!("{fraction:0<6}").parse().map_err(|_| bad())?;
    let micros = whole
        .checked_mul(1_000_000)
        .and_then(|m| m.checked_add(fraction))
        .ok_or_else(bad)?;
    if micros == 0 {
        return Err("the budget must be more than zero".into());
    }
    Ok(micros)
}

#[cfg(test)]
mod tests;
