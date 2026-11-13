//! The team table. Each team is a whole tracker of its own: its fold, writer
//! lease, Felix connections and tokens. Nothing in one team's tracker reaches
//! another's, so a lost lease, a log of a newer format or a slow Felix stays in
//! the team it happened to. See "One tracker, many teams" in docs/design.md.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::Result;
use kbl_core::org::TeamName;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;

use crate::config::{Caller, Tokens};
use crate::felix::Broker;
use crate::tracker::Tracker;

/// Calls in flight per team when `KBL_TEAM_INFLIGHT` is not set.
pub(crate) const DEFAULT_INFLIGHT: usize = 64;

pub(crate) struct Team<B> {
    name: TeamName,
    tracker: Tracker<B>,
    tokens: Tokens,
    inflight: Arc<Semaphore>,
    /// `work_next` waits for work while it holds a slot, so it has a pool of its
    /// own: agents waiting for work cannot take the slots a holder needs to
    /// extend or finish.
    waiting: Arc<Semaphore>,
}

impl<B: Broker> Team<B> {
    pub(crate) fn new(
        name: TeamName,
        tracker: Tracker<B>,
        tokens: Tokens,
        inflight: usize,
    ) -> Self {
        Self {
            name,
            tracker,
            tokens,
            inflight: Arc::new(Semaphore::new(inflight.max(1))),
            waiting: Arc::new(Semaphore::new(inflight.max(1))),
        }
    }
}

impl<B> Team<B> {
    pub(crate) fn name(&self) -> &TeamName {
        &self.name
    }

    pub(crate) fn tracker(&self) -> &Tracker<B> {
        &self.tracker
    }

    /// A slot for one call, or none when the team is at its cap. It does not
    /// wait: a team at its cap answers busy at once rather than queueing work
    /// that holds a connection open.
    pub(crate) fn admit(&self, waits: bool) -> Option<OwnedSemaphorePermit> {
        let pool = if waits { &self.waiting } else { &self.inflight };
        pool.clone().try_acquire_owned().ok()
    }
}

pub(crate) struct Teams<B>(BTreeMap<TeamName, Arc<Team<B>>>);

impl<B: Broker> Teams<B> {
    /// Refuses two teams of one name, and a token that would name two callers:
    /// an agent or ingest token is bound to one team, and a person token in
    /// several teams must be the same person in each.
    pub(crate) fn new(teams: Vec<Team<B>>) -> Result<Self> {
        let mut table = BTreeMap::new();
        let mut seen: HashMap<&str, (&TeamName, Caller)> = HashMap::new();
        for team in &teams {
            for (token, caller) in team.tokens.entries() {
                if let Some((other, before)) = seen.insert(token, (&team.name, caller.clone())) {
                    let same_person = matches!(
                        (&before, &caller),
                        (Caller::Person(a), Caller::Person(b)) if a == b
                    );
                    anyhow::ensure!(
                        same_person,
                        "a token is listed in teams {other} and {}, and is not one person in both",
                        team.name
                    );
                }
            }
        }
        for team in teams {
            let name = team.name.clone();
            anyhow::ensure!(
                table.insert(name.clone(), Arc::new(team)).is_none(),
                "team {name} is listed twice"
            );
        }
        Ok(Self(table))
    }

    /// Every team the token belongs to, with who it is there. Each team's
    /// tokens are compared in full, so the time taken does not say where a
    /// token matched.
    pub(crate) fn who(&self, token: &str) -> Vec<(Arc<Team<B>>, Caller)> {
        let mut found = Vec::new();
        for team in self.0.values() {
            if let Some(caller) = team.tokens.who(token) {
                found.push((team.clone(), caller));
            }
        }
        found
    }

    #[cfg(test)]
    pub(crate) fn get(&self, name: &str) -> Option<Arc<Team<B>>> {
        self.0.values().find(|t| t.name.as_str() == name).cloned()
    }

    /// Runs each team's lease renewal and sweep. A team that loses its lease
    /// for good stops here and answers 503 from then on; the others keep
    /// serving. Returns when no team holds its lease, or at once when a team's
    /// maintenance panicked: that team would go on writing with no sweep.
    pub(crate) async fn maintain(&self) {
        let mut running = JoinSet::new();
        for team in self.0.values() {
            let (name, tracker) = (team.name.clone(), team.tracker.clone());
            running.spawn(async move {
                tracker.maintain().await;
                name
            });
        }
        while let Some(done) = running.join_next().await {
            match done {
                Ok(name) => eprintln!("kbl-tracker: team {name} lost its writer lease"),
                Err(e) => {
                    eprintln!("kbl-tracker: a team's maintenance failed: {e}");
                    return;
                }
            }
        }
    }
}
