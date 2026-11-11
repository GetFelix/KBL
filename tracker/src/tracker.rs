//! The tracker: the only writer of `work.events` and the ready streams, and the
//! claim path for agents. See docs/work-tracker.md.
//!
//! State is the fold of `work.events` ([`agents_core::work_fold`]). A record is
//! appended only if the fold accepts it, so the log never holds one the dashboard
//! would have to explain. `work_next` polls the ready tiers as a member named
//! after the agent and writes `claimed` before it answers: a delivery is never
//! held without a record, and `done` is written before the ack.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agents_core::codec::Codec;
use agents_core::names::{AgentName, ItemId, Key};
use agents_core::work::{
    Actor, ReadyRecord, TIERS, WORK_EVENTS, WorkLogged, WorkRecord, ready_stream,
};
use agents_core::work_fold::{Seek, Stage, State};
use felix_client::GroupRecord;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::felix::{Broker, ExtendError};

/// How often `work_next` looks again while it waits for work.
const POLL_EVERY: Duration = Duration::from_millis(200);
/// Longest a single `work_next` waits, so a host's tool timeout is not hit.
pub(crate) const MAX_WAIT: Duration = Duration::from_secs(30);
/// Records between the snapshots the startup fold keeps.
const SNAPSHOT_EVERY: usize = 1000;
const DEFAULT_TIER: u8 = 2;

#[derive(Debug)]
pub(crate) enum Error {
    /// The fold would not take the record. `why` is its reason in snake case.
    Refused { why: String },
    /// The caller does not hold the item.
    NotHeld(String),
    /// The claim lapsed or went to another agent.
    Lost(String),
    /// Felix failed. The call did not complete.
    Felix(anyhow::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Refused { why } => write!(f, "refused: {why}"),
            Error::NotHeld(m) | Error::Lost(m) => f.write_str(m),
            Error::Felix(e) => write!(f, "Felix failed: {e:#}"),
        }
    }
}

/// One agent's delivery.
struct Held {
    item: ItemId,
    attempt: u32,
    tier: u8,
    /// None when the hold comes from the log after a restart, with no delivery to settle.
    record: Option<GroupRecord>,
}

#[derive(Default)]
struct Core {
    state: State,
    /// The ready record each item is waiting on: tier and stamp.
    live: HashMap<ItemId, (u8, u64)>,
    held: HashMap<AgentName, Held>,
    /// Items whose claim the fold refused, with the log position of the refusal.
    /// They are published again only after the log moves, or every poll would
    /// publish, refuse and ack the same item forever.
    refused: HashMap<ItemId, Option<u64>>,
    /// A write failed and may have landed; read the log again before the next call.
    stale: bool,
}

struct Inner<B> {
    broker: B,
    core: Mutex<Core>,
    visibility: Duration,
    me: Key,
    stamp: AtomicU64,
    /// Dies after the first delivery is polled and before `claimed` is written. For the dev-stack check.
    crash_after_poll: bool,
}

pub(crate) struct Tracker<B>(Arc<Inner<B>>);

impl<B> Clone for Tracker<B> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<B: Broker> Tracker<B> {
    /// Rebuilds the state from the start of `work.events`, then publishes every
    /// ready item. Ready records already on the streams are left to be skipped
    /// as stale, since there is no telling which were acked.
    pub(crate) async fn start(
        broker: B,
        visibility: Duration,
        crash_after_poll: bool,
    ) -> anyhow::Result<Self> {
        let tracker = Self(Arc::new(Inner {
            broker,
            core: Mutex::new(Core::default()),
            visibility,
            me: Key::new("tracker").expect("a valid key"),
            stamp: AtomicU64::new(now_micros()),
            crash_after_poll,
        }));
        {
            let mut core = tracker.0.core.lock().await;
            tracker.load(&mut core, false).await?;
            tracker.sync_ready(&mut core).await;
            // Take back the deliveries Felix still holds for agents the log shows holding.
            let holders: Vec<AgentName> = core
                .state
                .items
                .values()
                .filter(|i| i.closed.is_none())
                .filter_map(|i| i.hold.as_ref())
                .filter_map(|h| AgentName::new(h.by.as_str()).ok())
                .collect();
            for agent in holders {
                tracker.recover(&mut core, &agent).await;
            }
        }
        Ok(tracker)
    }

    /// Claims the highest-priority ready item for `agent`, waiting up to `wait`.
    pub(crate) async fn next(&self, agent: &AgentName, wait: Duration) -> Result<Value, Error> {
        let deadline = Instant::now() + wait.min(MAX_WAIT);
        loop {
            {
                let mut core = self.0.core.lock().await;
                self.catch_up(&mut core).await?;
                self.reconcile(&mut core, agent).await;
                if !core.held.contains_key(agent)
                    && let Some(attempt) = core.state.held_by(agent.as_str())
                {
                    // The log says this agent holds an item after a restart. It never
                    // gets a second one; try again to take the delivery back.
                    if !self.recover(&mut core, agent).await {
                        let item = attempt.0;
                        core.held.insert(
                            agent.clone(),
                            Held {
                                item,
                                attempt: attempt.1,
                                tier: DEFAULT_TIER,
                                record: None,
                            },
                        );
                    }
                }
                if let Some(h) = core.held.get(agent) {
                    return Ok(claimed(&core.state, h, true));
                }
                self.sync_ready(&mut core).await;
                if let Some(h) = self.try_claim(&mut core, agent).await? {
                    let reply = claimed(&core.state, &h, false);
                    core.held.insert(agent.clone(), h);
                    return Ok(reply);
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(json!({ "claimed": false }));
            }
            tokio::time::sleep(left.min(POLL_EVERY)).await;
        }
    }

    /// Keeps the agent's delivery standing for another visibility period.
    pub(crate) async fn extend(&self, agent: &AgentName, item: &ItemId) -> Result<Value, Error> {
        let (stream, record) = {
            let mut core = self.0.core.lock().await;
            self.catch_up(&mut core).await?;
            if self.reconcile(&mut core, agent).await.as_ref() == Some(item) {
                return Err(lapsed(item));
            }
            match core.held.get(agent) {
                Some(h) if &h.item == item => match &h.record {
                    Some(r) => (ready_stream(h.tier), r.clone()),
                    None => return Err(restarted(item)),
                },
                _ if core.state.items.get(item).is_some_and(|i| holds(i, agent)) => {
                    return Err(restarted(item));
                }
                _ => return Err(lapsed(item)),
            }
        };
        match self
            .0
            .broker
            .extend(&stream, agent.as_str(), &record, self.0.visibility)
            .await
        {
            Ok(visible) => Ok(json!({
                "item": item,
                "deadline_ms": now_ms() + visible.as_millis() as u64,
            })),
            Err(ExtendError::Stale) => {
                let mut core = self.0.core.lock().await;
                if core.held.get(agent).is_some_and(|h| &h.item == item) {
                    core.held.remove(agent);
                }
                Err(lapsed(item))
            }
            Err(ExtendError::Failed(e)) => Err(Error::Felix(e)),
        }
    }

    /// Writes `done`, then acks the delivery. `cost_micros` is what the agent reports.
    pub(crate) async fn complete(
        &self,
        agent: &AgentName,
        item: &ItemId,
        cost_micros: u64,
    ) -> Result<Value, Error> {
        let mut core = self.0.core.lock().await;
        self.catch_up(&mut core).await?;
        if self.reconcile(&mut core, agent).await.as_ref() == Some(item) {
            return Err(lapsed(item));
        }
        let attempt = holding(&core, agent, item)?;
        self.append(
            &mut core,
            Actor::Agent,
            agent_key(agent),
            WorkRecord::Done {
                item: item.clone(),
                attempt,
                cost_micros,
            },
        )
        .await?;
        let warning = self.settle(&mut core, agent, item).await;
        self.sync_ready(&mut core).await;
        Ok(with_warning(
            json!({ "item": item, "status": "done", "cost_micros": cost_micros }),
            warning,
        ))
    }

    /// Writes `released`, then hands the delivery back so another agent can take it.
    pub(crate) async fn release(
        &self,
        agent: &AgentName,
        item: &ItemId,
        reason: Option<String>,
        cost_micros: u64,
    ) -> Result<Value, Error> {
        let mut core = self.0.core.lock().await;
        self.catch_up(&mut core).await?;
        if self.reconcile(&mut core, agent).await.as_ref() == Some(item) {
            return Err(lapsed(item));
        }
        let attempt = holding(&core, agent, item)?;
        self.append(
            &mut core,
            Actor::Agent,
            agent_key(agent),
            WorkRecord::Released {
                item: item.clone(),
                attempt,
                reason,
                cost_micros,
            },
        )
        .await?;
        let warning = self.settle(&mut core, agent, item).await;
        self.sync_ready(&mut core).await;
        Ok(with_warning(
            json!({ "item": item, "status": "released" }),
            warning,
        ))
    }

    /// Writes `gave_up`. The item may be retried once, like a release.
    pub(crate) async fn give_up(
        &self,
        agent: &AgentName,
        item: &ItemId,
        reason: String,
        cost_micros: u64,
    ) -> Result<Value, Error> {
        let mut core = self.0.core.lock().await;
        self.catch_up(&mut core).await?;
        if self.reconcile(&mut core, agent).await.as_ref() == Some(item) {
            return Err(lapsed(item));
        }
        let attempt = holding(&core, agent, item)?;
        self.append(
            &mut core,
            Actor::Agent,
            agent_key(agent),
            WorkRecord::GaveUp {
                item: item.clone(),
                attempt,
                reason,
                cost_micros,
            },
        )
        .await?;
        let warning = self.settle(&mut core, agent, item).await;
        self.sync_ready(&mut core).await;
        Ok(with_warning(
            json!({ "item": item, "status": "gave_up" }),
            warning,
        ))
    }

    /// Appends a record on behalf of a person, if the fold accepts it.
    pub(crate) async fn person_record(
        &self,
        person: Key,
        record: WorkRecord,
    ) -> Result<Value, Error> {
        let mut core = self.0.core.lock().await;
        self.catch_up(&mut core).await?;
        let kind = record.kind();
        let offset = self
            .append(&mut core, Actor::Person, person, record)
            .await?;
        self.sync_ready(&mut core).await;
        Ok(json!({ "kind": kind, "offset": offset }))
    }

    /// Polls the tiers in order and returns the first delivery that turns into a claim.
    async fn try_claim(&self, core: &mut Core, agent: &AgentName) -> Result<Option<Held>, Error> {
        for tier in 0..TIERS {
            let stream = ready_stream(tier);
            loop {
                let polled = self
                    .0
                    .broker
                    .poll(&stream, agent.as_str(), self.0.visibility)
                    .await
                    .map_err(Error::Felix)?;
                let Some(record) = polled else { break };
                if let Some(held) = self.take_delivery(core, agent, tier, record).await? {
                    return Ok(Some(held));
                }
            }
        }
        Ok(None)
    }

    /// Turns one delivery into a claim, or acks it when it no longer describes
    /// claimable work.
    async fn take_delivery(
        &self,
        core: &mut Core,
        agent: &AgentName,
        tier: u8,
        record: GroupRecord,
    ) -> Result<Option<Held>, Error> {
        let stream = ready_stream(tier);
        let Ok(ready) = ReadyRecord::decode(&record.payload) else {
            eprintln!(
                "agents-tracker: {stream} offset {} is not a ready record; acked",
                record.offset
            );
            self.ack_quiet(&stream, agent, record.offset).await;
            return Ok(None);
        };
        let item = ready.item.clone();
        // A newer record for the item (another tier, or published after a restart) wins.
        // No entry means this is the only record the tracker knows of, so it is adopted.
        match core.live.get(&item) {
            Some(&(t, s)) if (t, s) != (ready.tier, ready.stamp) => {
                self.ack_quiet(&stream, agent, record.offset).await;
                return Ok(None);
            }
            Some(_) => {}
            None => {
                core.live.insert(item.clone(), (ready.tier, ready.stamp));
            }
        }
        if ready.tier != tier {
            self.ack_quiet(&stream, agent, record.offset).await;
            return Ok(None);
        }
        match core.state.stage(&item) {
            Some(Stage::Ready) => {}
            Some(Stage::Held) => {
                let Some(hold) = core.state.items.get(&item).and_then(|i| i.hold.clone()) else {
                    return Ok(None);
                };
                if hold.by.as_str() == agent.as_str() {
                    // The agent's own claim, back after a restart. It is already logged.
                    return Ok(Some(Held {
                        item,
                        attempt: hold.attempt,
                        tier,
                        record: Some(record),
                    }));
                }
                // Felix handed the item out again, so the old claim lapsed.
                if let Err(e) = self
                    .append(
                        core,
                        Actor::Tracker,
                        self.0.me.clone(),
                        WorkRecord::Released {
                            item: item.clone(),
                            attempt: hold.attempt,
                            reason: Some("lapsed".into()),
                            cost_micros: 0,
                        },
                    )
                    .await
                {
                    self.nack_quiet(&stream, agent, &record).await;
                    return Err(e);
                }
                core.held.retain(|_, h| h.item != item);
            }
            _ => {
                core.live.remove(&item);
                self.ack_quiet(&stream, agent, record.offset).await;
                return Ok(None);
            }
        }
        if self.0.crash_after_poll {
            std::process::exit(70);
        }
        let attempt = core.state.items[&item].next_attempt();
        let claim = WorkRecord::Claimed {
            item: item.clone(),
            attempt,
            deadline_ms: now_ms() + self.0.visibility.as_millis() as u64,
        };
        match self
            .append(core, Actor::Agent, agent_key(agent), claim)
            .await
        {
            Ok(_) => Ok(Some(Held {
                item,
                attempt,
                tier,
                record: Some(record),
            })),
            Err(Error::Refused { why }) => {
                // Over budget or out of tries: a person decides, and a redo publishes again.
                eprintln!("agents-tracker: {item} not claimable ({why}); skipped");
                core.live.remove(&item);
                core.refused.insert(item, core.state.at);
                self.ack_quiet(&stream, agent, record.offset).await;
                Ok(None)
            }
            Err(e) => {
                self.nack_quiet(&stream, agent, &record).await;
                Err(e)
            }
        }
    }

    /// After done, release or give-up: ack the delivery if the item is not ready
    /// again, otherwise hand it back so another agent can take it.
    async fn settle(&self, core: &mut Core, agent: &AgentName, item: &ItemId) -> Option<String> {
        let held = core.held.remove(agent);
        let ready = core.state.stage(item) == Some(Stage::Ready);
        if !ready {
            core.live.remove(item);
        }
        let Held {
            tier,
            record: Some(record),
            ..
        } = held?
        else {
            return None;
        };
        let stream = ready_stream(tier);
        let settled = if ready {
            self.0.broker.nack(&stream, agent.as_str(), &record).await
        } else {
            self.0
                .broker
                .ack(&stream, agent.as_str(), record.offset)
                .await
        };
        settled
            .err()
            .map(|e| format!("the work is recorded but the delivery was not settled: {e:#}"))
    }

    /// The fold is the truth for holds. If it no longer shows `agent` holding what
    /// the tracker has in `held` (a person closed it, an attempt was closed, another
    /// agent took it), the hold is dropped and the delivery settled. Returns the item.
    async fn reconcile(&self, core: &mut Core, agent: &AgentName) -> Option<ItemId> {
        let h = core.held.get(agent)?;
        let shown = core
            .state
            .items
            .get(&h.item)
            .and_then(|i| i.hold.as_ref())
            .is_some_and(|x| x.by.as_str() == agent.as_str() && x.attempt == h.attempt);
        if shown {
            return None;
        }
        let h = core.held.remove(agent)?;
        let ready = core.state.stage(&h.item) == Some(Stage::Ready);
        if !ready {
            core.live.remove(&h.item);
        }
        if let Some(record) = h.record {
            let stream = ready_stream(h.tier);
            if ready {
                self.nack_quiet(&stream, agent, &record).await;
            } else {
                self.ack_quiet(&stream, agent, record.offset).await;
            }
        }
        Some(h.item)
    }

    /// Takes back the delivery Felix still holds for `agent`, whose first poll on
    /// its connection reclaims it. True when the held item's delivery was found.
    /// Any other delivery the agent had is handed back.
    async fn recover(&self, core: &mut Core, agent: &AgentName) -> bool {
        let Some((item, attempt)) = core.state.held_by(agent.as_str()) else {
            return false;
        };
        let mut found = false;
        let mut others = Vec::new();
        // The broker reclaims in a connection's first poll, so that poll goes to
        // the tier the item was published on.
        let first = core
            .state
            .items
            .get(&item)
            .and_then(|i| i.tier)
            .unwrap_or(DEFAULT_TIER)
            .min(TIERS - 1);
        for tier in std::iter::once(first).chain((0..TIERS).filter(|t| *t != first)) {
            let stream = ready_stream(tier);
            // Bounded, in case the broker keeps handing records back.
            for _ in 0..64 {
                let Ok(Some(record)) = self
                    .0
                    .broker
                    .poll(&stream, agent.as_str(), self.0.visibility)
                    .await
                else {
                    break;
                };
                match ReadyRecord::decode(&record.payload) {
                    Ok(ready) if ready.item == item && !found => {
                        core.live.insert(item.clone(), (ready.tier, ready.stamp));
                        core.held.insert(
                            agent.clone(),
                            Held {
                                item: item.clone(),
                                attempt,
                                tier,
                                record: Some(record),
                            },
                        );
                        found = true;
                    }
                    _ => others.push((stream.clone(), record)),
                }
            }
        }
        for (stream, record) in others {
            self.nack_quiet(&stream, agent, &record).await;
        }
        found
    }

    /// Publishes a ready record for every Ready item that has none for its tier.
    async fn sync_ready(&self, core: &mut Core) {
        let want: Vec<(ItemId, u8)> = core
            .state
            .items
            .iter()
            .filter(|(id, _)| core.state.stage(id) == Some(Stage::Ready))
            .filter(|(id, _)| core.refused.get(*id) != Some(&core.state.at))
            .map(|(id, it)| (id.clone(), it.tier.unwrap_or(DEFAULT_TIER).min(TIERS - 1)))
            .filter(|(id, tier)| core.live.get(id).is_none_or(|(t, _)| t != tier))
            .collect();
        for (item, tier) in want {
            let stamp = self.0.stamp.fetch_add(1, Ordering::Relaxed);
            let record = ReadyRecord {
                item: item.clone(),
                tier,
                stamp,
            };
            match self
                .0
                .broker
                .publish(&ready_stream(tier), record.encode())
                .await
            {
                Ok(_) => {
                    core.live.insert(item, (tier, stamp));
                }
                // The next sync tries again.
                Err(e) => eprintln!("agents-tracker: could not publish {item} as ready: {e:#}"),
            }
        }
    }

    /// Appends `record` if the fold accepts it, and applies it to the live state.
    async fn append(
        &self,
        core: &mut Core,
        actor: Actor,
        by: Key,
        record: WorkRecord,
    ) -> Result<u64, Error> {
        let logged = WorkLogged {
            at_ms: now_ms(),
            by,
            actor,
            record,
        };
        // The tracker is the only writer, so the next offset is the one after the
        // last applied. It can skip (generation starts), which the check below catches.
        let guess = core.state.at.map_or(0, |a| a + 1);
        // Applied before the write: a refusal changes nothing, and a failed write
        // marks the state stale so the next call rebuilds it from the log.
        if let Err(why) = core.state.try_apply(guess, &logged) {
            let why = serde_json::to_value(why)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| "invalid".into());
            return Err(Error::Refused { why });
        }
        match self.0.broker.publish(WORK_EVENTS, logged.encode()).await {
            Ok(offset) => {
                if offset != guess {
                    self.load(core, true).await.map_err(Error::Felix)?;
                }
                Ok(offset)
            }
            Err(e) => {
                core.stale = true;
                Err(Error::Felix(e))
            }
        }
    }

    /// Reads the log again if a write failed and may have landed.
    async fn catch_up(&self, core: &mut Core) -> Result<(), Error> {
        if core.stale {
            self.load(core, true).await.map_err(Error::Felix)?;
        }
        Ok(())
    }

    /// Folds the records after the last one applied, or all of them when `fresh`. The first load uses seek
    /// snapshots, which also bounds the replay of a long log.
    async fn load(&self, core: &mut Core, fresh: bool) -> anyhow::Result<()> {
        if fresh {
            core.state = State::default();
        }
        let from = core.state.at.map_or(0, |a| a + 1);
        let mut entries = Vec::new();
        for (offset, payload) in self.0.broker.read_from(WORK_EVENTS, from).await? {
            match WorkLogged::decode(&payload) {
                Ok(rec) => entries.push((offset, rec)),
                Err(e) => eprintln!("agents-tracker: {WORK_EVENTS} offset {offset}: {e}; skipped"),
            }
        }
        if core.state.at.is_none() {
            if let Some((last, _)) = entries.last() {
                let seek = Seek::build(&entries, SNAPSHOT_EVERY);
                core.state = seek.state_at(&entries, *last);
            }
        } else {
            for (offset, rec) in &entries {
                core.state.apply(*offset, rec);
            }
        }
        core.stale = false;
        Ok(())
    }

    async fn ack_quiet(&self, stream: &str, member: &AgentName, offset: u64) {
        if let Err(e) = self.0.broker.ack(stream, member.as_str(), offset).await {
            eprintln!("agents-tracker: ack of {stream} offset {offset} failed: {e:#}");
        }
    }

    async fn nack_quiet(&self, stream: &str, member: &AgentName, record: &GroupRecord) {
        if let Err(e) = self.0.broker.nack(stream, member.as_str(), record).await {
            eprintln!(
                "agents-tracker: nack of {stream} offset {} failed: {e:#}",
                record.offset
            );
        }
    }
}

fn holds(item: &agents_core::work_fold::Item, agent: &AgentName) -> bool {
    item.hold
        .as_ref()
        .is_some_and(|h| h.by.as_str() == agent.as_str())
}

/// The attempt `agent` holds `item` on: from its delivery, or from the log when
/// the tracker restarted since the claim.
fn holding(core: &Core, agent: &AgentName, item: &ItemId) -> Result<u32, Error> {
    if let Some(h) = core.held.get(agent) {
        return if &h.item == item {
            Ok(h.attempt)
        } else {
            Err(Error::NotHeld(format!("you hold {}, not {item}", h.item)))
        };
    }
    match core.state.items.get(item) {
        Some(i) if holds(i, agent) => Ok(i.hold.as_ref().map_or(1, |h| h.attempt)),
        // Someone else holds it or it is closed: the claim was lost.
        Some(i) if i.hold.is_some() || i.closed.is_some() => Err(lapsed(item)),
        _ => Err(Error::NotHeld(format!("you do not hold {item}"))),
    }
}

fn lapsed(item: &ItemId) -> Error {
    Error::Lost(format!(
        "your claim on {item} lapsed and may have gone to another agent. Stop working on it; use work_next for new work"
    ))
}

fn restarted(item: &ItemId) -> Error {
    Error::Lost(format!(
        "the tracker restarted after you claimed {item}, so it cannot extend that claim. It will lapse; use work_next for new work"
    ))
}

fn claimed(state: &State, held: &Held, resumed: bool) -> Value {
    let item = state.items.get(&held.item);
    json!({
        "claimed": true,
        "resumed": resumed,
        "attempt": held.attempt,
        "item": {
            "id": held.item,
            "title": item.map(|i| i.title.as_str()),
            "work": item.map(|i| i.work),
            "tier": item.and_then(|i| i.tier),
            "parent": item.and_then(|i| i.parent.as_ref()),
            "root": item.map(|i| &i.root),
            "branch": item.and_then(|i| i.branch.as_ref()),
            "pr": item.and_then(|i| i.pr.as_ref()),
            "context": item.map(|i| i.context.keys().collect::<Vec<_>>()),
        },
        "deadline_ms": item.and_then(|i| i.hold.as_ref()).map(|h| h.deadline_ms),
    })
}

fn with_warning(mut reply: Value, warning: Option<String>) -> Value {
    if let Some(w) = warning {
        reply["warning"] = w.into();
    }
    reply
}

fn agent_key(agent: &AgentName) -> Key {
    Key::new(agent.as_str()).expect("an agent name is a key")
}

/// Microseconds, so a restarted tracker's stamps start past the old one's.
fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests;
