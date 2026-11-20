//! Delegated workers: agent names a supervisor's token registers, each with
//! its own token, acting with the supervisor's grants and under its caps.

use kbl_core::org::TokenHash;
use kbl_core::work::{MAX_WORKERS_PER_SUPERVISOR, SUPERVISOR_GONE};
use kbl_core::work_fold::{State, Worker};

use super::*;

/// The reason on the release of an item a worker still held when it
/// finished. It abandoned the item, so unlike `supervisor_gone` it spends the try.
pub(crate) const WORKER_FINISHED: &str = "worker_finished";

/// Longest vendor, profile or model name a worker records.
pub(crate) const MAX_WORKER_LABEL: usize = 64;

/// Worker token hash to the worker, for the live workers only.
pub(super) type WorkerTokens = HashMap<String, LiveWorker>;

/// Who a worker's token names: the worker, and the supervisor identity it
/// acts for, which is the supervisor's name and its token hash together.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveWorker {
    pub(crate) worker: AgentName,
    pub(crate) supervisor: AgentName,
    pub(crate) supervisor_token: TokenHash,
}

/// What a supervisor asks for when it starts a worker.
pub(crate) struct NewWorker {
    pub(crate) worker: AgentName,
    pub(crate) vendor: Option<String>,
    pub(crate) profile: Option<String>,
    pub(crate) model: Option<String>,
}

/// Control characters, and the format characters that hide or reorder text.
fn unprintable(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn label(what: &str, v: &Option<String>) -> Result<(), Error> {
    match v {
        Some(s) if s.is_empty() || s.len() > MAX_WORKER_LABEL || s.chars().any(unprintable) => {
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
    /// Registers a worker for `supervisor`, whose own token hashes to
    /// `supervisor_token`, and returns the worker's token, which is shown here
    /// once and logged only as its hash.
    pub(crate) async fn start_worker(
        &self,
        supervisor: &AgentName,
        supervisor_token: &TokenHash,
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
            supervisor_token_sha256: supervisor_token.as_str().to_owned(),
            vendor: new.vendor,
            profile: new.profile,
            model: new.model,
        };
        let offset = self.append(&mut core, Actor::Agent, by, record).await?;
        Ok(json!({ "kind": "worker_started", "offset": offset, "worker": worker, "token": token }))
    }

    /// Stops a worker: its token stops working at once and the item it holds
    /// goes back in the same call, at no cost to its tries.
    pub(crate) async fn stop_worker(
        &self,
        actor: Actor,
        by: Key,
        worker: Key,
    ) -> Result<Value, Error> {
        self.begin().await?;
        let mut core = self.0.core.lock().await;
        let record = WorkRecord::WorkerStopped {
            worker: worker.clone(),
        };
        let offset = self.append(&mut core, actor, by, record).await?;
        let released = self.release_held(&mut core, &worker).await;
        drop(core);
        let released = released_json(&worker, released);
        self.sync_ready().await;
        // A release that failed after the stop landed is retried by the next sweep.
        Ok(
            json!({ "kind": "worker_stopped", "offset": offset, "released": released.0, "release_error": released.1 }),
        )
    }

    /// A worker reports it is done, with its own token: it ends like a stop,
    /// and an item it still holds goes back.
    pub(crate) async fn finish_worker(&self, worker: &AgentName) -> Result<Value, Error> {
        self.begin().await?;
        let by = Key::new(worker.as_str()).expect("an agent name is a key");
        let mut core = self.0.core.lock().await;
        let offset = self
            .append(
                &mut core,
                Actor::Agent,
                by.clone(),
                WorkRecord::WorkerFinished,
            )
            .await?;
        let released = self.release_held(&mut core, &by).await;
        drop(core);
        let released = released_json(&by, released);
        self.sync_ready().await;
        Ok(
            json!({ "kind": "worker_finished", "offset": offset, "released": released.0, "release_error": released.1 }),
        )
    }

    /// A supervisor records that the processes of these workers are running.
    /// Names that are not its live workers are left out, since a worker may
    /// have ended since the supervisor looked; nothing is written when none
    /// is left.
    pub(crate) async fn workers_alive(
        &self,
        supervisor: &AgentName,
        names: Vec<String>,
    ) -> Result<Value, Error> {
        if names.len() > MAX_WORKERS_PER_SUPERVISOR {
            return Err(Error::Invalid(format!(
                "at most {MAX_WORKERS_PER_SUPERVISOR} workers"
            )));
        }
        let names = names
            .iter()
            .map(|n| Key::new(n).map_err(|_| Error::Invalid(format!("{n} is not a worker name"))))
            .collect::<Result<Vec<_>, _>>()?;
        self.begin().await?;
        let by = Key::new(supervisor.as_str()).expect("an agent name is a key");
        let mut core = self.0.core.lock().await;
        let mut workers: Vec<Key> = core
            .state
            .live_workers(&by)
            .map(|(k, _)| k.clone())
            .filter(|k| names.contains(k))
            .collect();
        workers.dedup();
        if workers.is_empty() {
            return Ok(json!({ "kind": "worker_alive", "written": false }));
        }
        let record = WorkRecord::WorkerAlive { workers };
        let offset = self.append(&mut core, Actor::Agent, by, record).await?;
        Ok(json!({ "kind": "worker_alive", "written": true, "offset": offset }))
    }

    /// Gives back what an ended worker still holds, as a gone supervisor's
    /// workers do: no try is spent.
    async fn release_held(&self, core: &mut Core, worker: &Key) -> Result<Vec<ItemId>, Error> {
        // A worker that finished on its own abandoned what it held; one that
        // was stopped, or lost its supervisor, did not fail.
        let reason = if core.state.workers.get(worker).is_some_and(|w| w.finished) {
            WORKER_FINISHED
        } else {
            SUPERVISOR_GONE
        };
        let held: Vec<(ItemId, u32)> = core
            .state
            .items
            .values()
            .filter(|i| i.closed.is_none())
            .filter_map(|i| {
                let h = i.hold.as_ref()?;
                (&h.by == worker).then(|| (i.id.clone(), h.attempt))
            })
            .collect();
        let mut released = Vec::new();
        for (item, attempt) in held {
            let record = WorkRecord::Released {
                item: item.clone(),
                attempt,
                reason: Some(reason.into()),
                why: None,
                cost_micros: 0,
                usage: Vec::new(),
                floor_micros: self.floor(),
            };
            self.append(core, Actor::Tracker, self.0.me.clone(), record)
                .await?;
            core.owner.remove(&item);
            released.push(item);
        }
        Ok(released)
    }

    /// Releases the holds of workers that ended while the release did not
    /// land, such as a tracker that died between the two writes.
    pub(super) async fn release_stopped_holds(&self) {
        let stopped: Vec<Key> = {
            let core = self.0.core.lock().await;
            core.state
                .workers
                .iter()
                .filter(|(name, w)| {
                    w.stopped_ms.is_some() && core.state.held_by(name.as_str()).is_some()
                })
                .map(|(name, _)| name.clone())
                .collect()
        };
        for worker in stopped {
            let mut core = self.0.core.lock().await;
            let _ = self.release_held(&mut core, &worker).await;
        }
    }

    /// The workers `supervisor` started, or every worker for a person: what
    /// each holds and when it stopped.
    /// The team's flake list with each entry's hit count.
    pub(crate) async fn flakes(&self) -> Value {
        let core = self.0.core.lock().await;
        json!({ "flakes": super::ingest::flake_list(&core.state) })
    }

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
                    "finished": w.finished,
                    "alive_ms": w.alive_ms,
                    "holds": holds.map(|(item, attempt)| json!({ "item": item, "attempt": attempt })),
                })
            })
            .collect();
        json!({ "workers": list })
    }

    /// Stops every worker `which` picks, as for a supervisor that is gone:
    /// the token dies first, then each hold goes back at no cost to its tries.
    /// A worker already stopped that still holds an item is picked too, so a
    /// release that failed after its stop landed is retried. Answers the items
    /// released.
    pub(crate) async fn stop_workers(
        &self,
        which: impl Fn(&Worker) -> bool,
    ) -> Result<Vec<ItemId>, Error> {
        let picked: Vec<Key> = {
            let core = self.0.core.lock().await;
            core.state
                .workers
                .iter()
                .filter(|(name, w)| {
                    (w.stopped_ms.is_none() || core.state.held_by(name.as_str()).is_some())
                        && which(w)
                })
                .map(|(name, _)| name.clone())
                .collect()
        };
        if picked.is_empty() {
            return Ok(Vec::new());
        }
        self.begin().await?;
        let mut released = Vec::new();
        for worker in picked {
            let mut core = self.0.core.lock().await;
            let me = self.0.me.clone();
            // Checked again: a person or the supervisor may have stopped it meanwhile.
            let live = core
                .state
                .workers
                .get(&worker)
                .is_some_and(|w| w.stopped_ms.is_none());
            if live {
                let record = WorkRecord::WorkerStopped {
                    worker: worker.clone(),
                };
                self.append(&mut core, Actor::Tracker, me.clone(), record)
                    .await?;
            }
            released.extend(self.release_held(&mut core, &worker).await?);
        }
        self.sync_ready().await;
        Ok(released)
    }

    /// The live worker a token belongs to, with its supervisor.
    pub(crate) fn worker_by_token(&self, token: &str) -> Option<LiveWorker> {
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
                    LiveWorker {
                        worker: AgentName::agent_or_worker(name.as_str()).ok()?,
                        supervisor: AgentName::new(w.supervisor.as_str()).ok()?,
                        supervisor_token: TokenHash::parse(&w.supervisor_token_sha256).ok()?,
                    },
                ))
            })
            .collect();
        *self.0.workers.lock().unwrap_or_else(|e| e.into_inner()) = live;
    }
}

/// The items a release gave back, and the error text when it failed after the
/// stop or finish had landed; the next sweep releases what is left.
fn released_json(
    worker: &Key,
    released: Result<Vec<ItemId>, Error>,
) -> (Vec<ItemId>, Option<String>) {
    match released {
        Ok(items) => (items, None),
        Err(e) => {
            tracing::warn!(%worker, error = %e, "releasing an ended worker's hold failed; the next sweep retries");
            (Vec::new(), Some(e.to_string()))
        }
    }
}
