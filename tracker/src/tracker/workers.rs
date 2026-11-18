//! Delegated workers: agent names a supervisor's token registers, each with
//! its own token, acting with the supervisor's grants and under its caps.

use kbl_core::org::TokenHash;
use kbl_core::work_fold::State;

use super::*;

/// Longest vendor, profile or model name a worker records.
pub(crate) const MAX_WORKER_LABEL: usize = 64;

/// Worker token hash to (worker, supervisor), for the live workers only.
pub(super) type WorkerTokens = HashMap<String, (AgentName, AgentName)>;

/// What a supervisor asks for when it starts a worker.
pub(crate) struct NewWorker {
    pub(crate) worker: AgentName,
    pub(crate) vendor: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) model: Option<String>,
}

fn label(what: &str, v: &Option<String>) -> Result<(), Error> {
    match v {
        Some(s)
            if s.is_empty() || s.len() > MAX_WORKER_LABEL || s.chars().any(char::is_control) =>
        {
            Err(Error::Invalid(format!(
                "{what} must be 1 to {MAX_WORKER_LABEL} bytes with no control characters"
            )))
        }
        _ => Ok(()),
    }
}

fn worker_token() -> Result<String, Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| Error::Unavailable(format!("drawing a worker token: {e}")))?;
    Ok(format!(
        "kblw_{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

impl<B: Broker> Tracker<B> {
    /// Registers a worker for `supervisor` and returns its token, which is
    /// shown here once and logged only as its hash.
    pub(crate) async fn start_worker(
        &self,
        supervisor: &AgentName,
        new: NewWorker,
    ) -> Result<Value, Error> {
        label("vendor", &new.vendor)?;
        label("profile", &new.profile)?;
        label("model", &new.model)?;
        self.begin().await?;
        let token = worker_token()?;
        let by = Key::new(supervisor.as_str()).expect("an agent name is a key");
        let mut core = self.0.core.lock().await;
        if core.state.workers.contains_key(&by) {
            return Err(Error::Refused {
                why: "bad_worker".into(),
            });
        }
        let mine = core.state.live_workers(&by).count();
        let all = core
            .state
            .workers
            .values()
            .filter(|w| w.stopped_ms.is_none())
            .count();
        let s = &self.0.settings;
        if mine >= s.max_workers || all >= s.max_team_workers {
            return Err(Error::Refused {
                why: "worker_cap".into(),
            });
        }
        let worker = Key::new(new.worker.as_str()).expect("an agent name is a key");
        let record = WorkRecord::WorkerStarted {
            worker: worker.clone(),
            token_sha256: TokenHash::of(&token).as_str().to_owned(),
            vendor: new.vendor,
            profile: new.profile,
            model: new.model,
        };
        let offset = self.append(&mut core, Actor::Agent, by, record).await?;
        Ok(json!({ "kind": "worker_started", "offset": offset, "worker": worker, "token": token }))
    }

    /// Stops a worker: its token stops working at once, and the item it holds
    /// goes back when its claim runs out, as a crashed agent's would.
    pub(crate) async fn stop_worker(
        &self,
        actor: Actor,
        by: Key,
        worker: Key,
    ) -> Result<Value, Error> {
        self.begin().await?;
        let mut core = self.0.core.lock().await;
        let record = WorkRecord::WorkerStopped { worker };
        let offset = self.append(&mut core, actor, by, record).await?;
        Ok(json!({ "kind": "worker_stopped", "offset": offset }))
    }

    /// The workers `supervisor` started, or every worker for a person: what
    /// each holds and when it stopped.
    pub(crate) async fn workers(&self, supervisor: Option<&Key>) -> Value {
        let core = self.0.core.lock().await;
        let list: Vec<Value> = core
            .state
            .workers
            .iter()
            .filter(|(_, w)| supervisor.is_none_or(|s| &w.supervisor == s))
            .map(|(name, w)| {
                let holds = core.state.held_by(name.as_str());
                json!({
                    "worker": name,
                    "supervisor": w.supervisor,
                    "vendor": w.vendor,
                    "profile": w.profile,
                    "model": w.model,
                    "started_ms": w.started_ms,
                    "stopped_ms": w.stopped_ms,
                    "holds": holds.map(|(item, attempt)| json!({ "item": item, "attempt": attempt })),
                })
            })
            .collect();
        json!({ "workers": list })
    }

    /// The live worker a token belongs to, with its supervisor.
    pub(crate) fn worker_by_token(&self, token: &str) -> Option<(AgentName, AgentName)> {
        let hash = TokenHash::of(token);
        self.0
            .workers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(hash.as_str())
            .cloned()
    }

    /// The name whose grants `agent` acts with: its supervisor's when it is a worker.
    pub(super) fn grant_name<'a>(state: &'a State, agent: &'a AgentName) -> &'a str {
        Key::new(agent.as_str())
            .ok()
            .and_then(|k| state.workers.get(&k))
            .filter(|w| w.stopped_ms.is_none())
            .map_or(agent.as_str(), |w| w.supervisor.as_str())
    }

    /// The supervisor a record by `by` is written for, when `by` is a live worker.
    pub(super) fn supervisor_of(state: &State, actor: Actor, by: &Key) -> Option<Key> {
        (actor == Actor::Agent)
            .then(|| state.workers.get(by))
            .flatten()
            .filter(|w| w.stopped_ms.is_none())
            .map(|w| w.supervisor.clone())
    }

    /// Rebuilt from the state after every landed write and rebuild, so a
    /// token works only once its `worker_started` is on the log.
    pub(super) fn refresh_workers(&self, state: &State) {
        let live: WorkerTokens = state
            .workers
            .iter()
            .filter(|(_, w)| w.stopped_ms.is_none())
            .filter_map(|(name, w)| {
                Some((
                    w.token_sha256.clone(),
                    (
                        AgentName::new(name.as_str()).ok()?,
                        AgentName::new(w.supervisor.as_str()).ok()?,
                    ),
                ))
            })
            .collect();
        *self.0.workers.lock().unwrap_or_else(|e| e.into_inner()) = live;
    }
}
