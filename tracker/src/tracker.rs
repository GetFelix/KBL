//! The tracker: the only writer of `work.events` and the ready streams, and the
//! claim path for agents. See docs/work-tracker.md.
//!
//! State is the fold of `work.events` ([`kbl_core::work_fold`]). A record is
//! appended only if the fold accepts it, so the log never holds one the dashboard
//! would have to explain. `work_next` polls the ready tiers as a member named
//! after the agent and writes `claimed` before it answers: a delivery is never
//! held without a record, and `done` is written before the ack.
//!
//! Locks: `core` guards the fold and the delivery table and is held for state
//! changes and appends only; each agent has its own lock for its polls and
//! extends, so one slow Felix call stalls that agent and nobody else.
//! Every Felix call has a timeout.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use felix_client::GroupRecord;
use kbl_core::codec::Codec;
use kbl_core::content::Content;
use kbl_core::names::{AgentName, ItemId, Key};
use kbl_core::work::{
    Actor, LANES, PrChecks, PrPhase, REVIEW_LANES, ReadError, ReadyRecord, RefusalReason, TIERS,
    Usage, WORK_EVENTS, WorkLogged, WorkRecord, read, ready_stream, valid_review_tier,
};
use kbl_core::work_fold::{Stage, State};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::felix::{Broker, ConditionalWrite, ExtendError, holder_token};

mod ingest;
mod reads;
mod request_keys;

/// The work loop, in the reply to a claim. A host with no prompt of its own reads it there.
const LOOP_AFTER_CLAIM: &str = "Work this item to done, or to a question for a person, then call work_complete. Call work_next again. Stop when work_next says nothing is ready, or when you are asked to stop.";

/// The reply to `work_next` while the agent's own stale claim is being released.
const LOOP_RETRY: &str = "Call work_next again in a few seconds. If it keeps saying this, stop.";

/// The reply to `work_next` when there is nothing to start.
const LOOP_WHEN_EMPTY: &str = "Nothing is ready for you. End your session; do not poll in a loop.";

/// How often `work_next` looks again while it waits for work.
const POLL_EVERY: Duration = Duration::from_millis(200);
/// Longest a single `work_next` waits, so a host's tool timeout is not hit.
pub(crate) const MAX_WAIT: Duration = Duration::from_secs(30);
/// How long any one Felix call may take.
const FELIX_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one lease put or read may take. Short so that one hung put still
/// leaves two more renewals inside the lease.
const LEASE_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_TIER: u8 = 2;
/// An item to publish: its tier, the record it had, and whether that record lapsed.
type Want = (ItemId, u8, Option<(u8, u64)>, bool);
/// Records one poll of a lane looks at before it gives up for this call.
const MAX_LOOKED_AT: u32 = 64;
/// Visibility periods after which a ready record is published again. Felix dead-letters
/// at five handouts, and a handout cannot come sooner than one period after the last.
const RECORD_LAPSE_PERIODS: u32 = 6;
/// How long a tracker without the writer lease keeps running before it exits.
const FENCE_GRACE: Duration = Duration::from_secs(10);
/// How long an agent's broker connection stays open with no call from it.
/// Allowance for clock skew between the tracker and the lease's holder of record.
const LEASE_SKEW: Duration = Duration::from_secs(1);
const MEMBER_IDLE: Duration = Duration::from_secs(600);

pub(crate) struct Settings {
    /// The repos each agent may reach beyond its own tree.
    pub(crate) grants: crate::config::Grants,
    /// How long a claim stands without an extension.
    pub(crate) visibility: Duration,
    /// How long the writer lease stands without a renewal.
    pub(crate) lease_ttl: Duration,
    /// How long a starting tracker waits for another's lease to run out.
    pub(crate) lease_wait: Duration,
    /// Dies after the first delivery is polled and before `claimed` is written.
    /// For the dev-stack check.
    pub(crate) crash_after_poll: bool,
    /// Dies after a terminal record is written and before the answer. For the
    /// dev-stack check that a retried call is replayed.
    pub(crate) crash_after_done: bool,
    /// Reviewer tier names, one ready lane each. The first reads a request
    /// first and the last gives the final review.
    pub(crate) review_tiers: Vec<String>,
    /// The least a claim is charged, whatever usage the agent reports. Usage
    /// is the agent's word, so this is what bounds a tree's cost.
    pub(crate) cost_floor_micros: u64,
}

#[cfg(test)]
pub(crate) fn default_review_tiers() -> Vec<String> {
    vec!["fast".to_owned(), "deep".to_owned()]
}

impl Settings {
    /// Whether the reviewer tiers are names, distinct, and fit the lanes.
    pub(crate) fn check_review_tiers(&self) -> Result<(), String> {
        let t = &self.review_tiers;
        if t.is_empty() || t.len() > usize::from(REVIEW_LANES) {
            return Err(format!("name between 1 and {REVIEW_LANES} reviewer tiers"));
        }
        if let Some(bad) = t.iter().find(|n| !valid_review_tier(n)) {
            return Err(format!("{bad:?} is not a reviewer tier name"));
        }
        if t.iter().enumerate().any(|(i, n)| t[..i].contains(n)) {
            return Err("reviewer tiers must be different".into());
        }
        Ok(())
    }

    /// The lane a ready item is published on: its priority tier, or its
    /// reviewer tier's lane. None when a review names a tier that is not configured.
    fn lane(&self, it: &kbl_core::work_fold::Item) -> Option<u8> {
        match &it.review {
            Some(spec) => {
                let i = self.review_tiers.iter().position(|t| *t == spec.tier)?;
                Some(TIERS + u8::try_from(i).ok()?)
            }
            None => Some(it.tier.unwrap_or(DEFAULT_TIER).min(TIERS - 1)),
        }
    }
}

#[derive(Debug)]
pub(crate) enum Error {
    /// The fold would not take the record. `why` is its reason in snake case.
    Refused { why: String },
    /// The call is malformed or over a size limit; nothing was tried.
    Invalid(String),
    /// The caller does not hold the item.
    NotHeld(String),
    /// The claim lapsed or went to another agent.
    Lost(String),
    /// Felix failed. The call did not complete.
    Felix(anyhow::Error),
    /// The tracker cannot serve right now: no writer lease, or its state is
    /// being rebuilt from the log.
    Unavailable(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Refused { why } => write!(f, "refused: {why}"),
            Error::NotHeld(m) | Error::Lost(m) | Error::Unavailable(m) | Error::Invalid(m) => {
                f.write_str(m)
            }
            Error::Felix(e) => write!(f, "Felix failed: {e:#}"),
        }
    }
}

/// Which claim a terminal call is for. The claim's offset names one claim;
/// an attempt number is shared by every claim of a retried attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Which {
    Attempt(u32),
    /// The offset of the claim's `claimed` record.
    Claim(u64),
}

/// One agent's delivery of one ready record.
struct Delivery {
    /// Names this delivery among all the deliveries of the same item.
    id: u64,
    item: ItemId,
    attempt: u32,
    tier: u8,
    record: GroupRecord,
}

/// The delivery the tracker currently stands behind for an item. Only its
/// agent may settle it: a group ack names an offset, not a delivery, so an ack
/// from a stale holder would take the next holder's delivery.
struct Owner {
    agent: AgentName,
    id: u64,
    last_extended: Instant,
}

struct LeaseHeld {
    version: u64,
    valid_until: Instant,
}

#[derive(Default)]
struct Core {
    state: State,
    /// The offset after the last committed record read, skipped offsets
    /// included (a new shard leader's generation start). Never taken from a
    /// refusal: Felix's tail there counts offsets claimed but not committed,
    /// and writing past one would skip a record this tracker never folded.
    tail: u64,
    /// The ready record each item is waiting on: tier and stamp.
    live: HashMap<ItemId, (u8, u64)>,
    /// Since when each Ready item's `live` record has stood, and how often it was
    /// published again for lapsing. The entry starts when the item becomes Ready
    /// and is dropped when it is claimed or settled.
    published: HashMap<ItemId, (Instant, u32)>,
    owner: HashMap<ItemId, Owner>,
    /// Items whose claim the fold refused, with the log position of the refusal.
    /// They are published again only after the log moves, or every poll would
    /// publish, refuse and ack the same item forever.
    refused: HashMap<ItemId, Option<u64>>,
    /// Review lanes an agent polled and left with nothing after passing over its own
    /// reviews, by agent and tier. Polling again would pass them over again, and each
    /// pass is an ack and a fresh publish.
    emptied: HashMap<(String, u8), Emptied>,
    /// Items whose delivery a finished holder is handing back, with the settle
    /// count when the nack ended (`u64::MAX` while it runs). A group nack names
    /// an offset, not a delivery, so it also takes the lease of anyone Felix
    /// handed the record to before it landed (felix#2207).
    handed_back: HashMap<ItemId, u64>,
    recall: Recall,
    /// Agents whose token was revoked. A claim checks it under this lock, so a
    /// `work_next` that passed the token check before the revoke cannot claim
    /// after the revoke has released the agent's holds.
    revoked: HashSet<AgentName>,
    /// Agents whose delivery a takeover could not look for because Felix failed,
    /// with when the sweep may free their hold anyway: one visibility after the
    /// first failure, when Felix has let the delivery lapse too.
    recovering: HashMap<String, Recovering>,
}

/// A hold whose delivery a takeover could not look for yet.
struct Recovering {
    /// When the sweep may free the hold anyway.
    until: Instant,
    /// The lanes already searched, one bit per tier.
    searched: u8,
}

/// The writer lease and whether the state may be written on. Kept apart from
/// [`Core`], whose lock an append holds for a whole Felix call: renewal must not
/// wait behind one. Locked briefly and never across an await.
#[derive(Default)]
struct LeaseState {
    held: Option<LeaseHeld>,
    /// Another tracker took the writer lease.
    fenced: bool,
    /// The state may not match the log: rebuild it before serving.
    stale: bool,
    /// The lease was taken again since the last rebuild, so the state may miss
    /// another tracker's records. Unlike an offset skip, no append may use it.
    retaken: bool,
    /// Counts retakes, so a rebuild that read before one does not clear `retaken`.
    retakes: u64,
    /// Counts conditional appends Felix refused. One past `confirmed` means
    /// another tracker may hold the lease, whatever our view of it says.
    refusals: u64,
    /// `refusals` as of the last renewal Felix granted by version.
    confirmed: u64,
}

impl LeaseState {
    /// The refusals a lease put sent now answers for: only those whose state
    /// was rebuilt before it. A grant before the rebuild says nothing about
    /// records that land during it.
    fn answerable(&self) -> u64 {
        if self.stale {
            self.confirmed
        } else {
            self.refusals
        }
    }

    /// Felix refused a conditional write, or its reply was lost: rebuild the
    /// state and confirm the lease by version before the next write.
    fn refused(&mut self) {
        self.stale = true;
        self.refusals += 1;
    }
}

/// What a review lane looked like when an agent last left it with nothing to take.
struct Emptied {
    at: Option<u64>,
    /// The published items in the lane that are not the agent's own work.
    others: Vec<ItemId>,
    /// A record that is leased or dead-lettered in the lane can come free without a
    /// change here, so the mark lapses after a visibility period.
    until: Instant,
}

/// GitHub reads `owner/repo` in any case, so `getfelix/felix#12` and
/// `GetFelix/felix#12` name one request. A progress record takes the casing an
/// open item already uses, so the fold's exact compare sees one request: another
/// item's is refused as `pr_in_use`, and the item's own keeps its status.
/// Rewritten before the append rather than compared in the fold, so the fold of
/// an existing log does not change. On an old log where two open items hold
/// one request in two casings, both are refused until one closes.
fn canonical_pr(state: &State, record: &mut WorkRecord) {
    let WorkRecord::Progress {
        item, pr: Some(pr), ..
    } = record
    else {
        return;
    };
    let found = state
        .items
        .values()
        .filter(|o| o.closed.is_none() && o.id != *item)
        .chain(state.items.get(item))
        .filter_map(|o| o.pr.as_ref())
        .find(|p| p.eq_ignore_ascii_case(pr));
    if let Some(p) = found {
        *pr = p.clone();
    }
}

/// Refuses evidence outside the record's own tree, or on another stream: a
/// person reads cited records as support. Checked before the append rather
/// than in the fold, so the fold of an existing log does not change.
fn evidence_in_tree(core: &Core, record: &WorkRecord, next: u64) -> Result<(), Error> {
    let root = |item: &ItemId| core.state.items.get(item).map(|i| &i.root);
    // An unknown item is the fold's to refuse.
    let Some(own) = record.item().and_then(root) else {
        return Ok(());
    };
    for e in record.evidence() {
        if e.stream != WORK_EVENTS {
            return Err(Error::Refused {
                why: "evidence_stream".into(),
            });
        }
        // A later offset is the fold's to refuse, as evidence from the future.
        if e.offset >= next {
            continue;
        }
        let in_tree = core.recall.subjects.get(&e.offset).and_then(root) == Some(own)
            // A root made by a record in another tree, such as a deferred
            // finding, points at that record from its own context.
            || core
                .state
                .items
                .get(own)
                .is_some_and(|r| r.context.values().any(|&o| o == e.offset));
        if !in_tree {
            return Err(Error::Refused {
                why: "evidence_outside_tree".into(),
            });
        }
    }
    Ok(())
}

/// Refuses a redo of an item whose pull request has a failed check unless it
/// names a note written since the check failed, so a failure is not rerun
/// unread. Checked before the append, like evidence, so the fold of an
/// existing log does not change.
fn retry_has_note(core: &Core, record: &WorkRecord) -> Result<(), Error> {
    let WorkRecord::ForkedFrom { item, note, .. } = record else {
        return Ok(());
    };
    // A closed, held or already redone item gets the fold's refusal, not a
    // request for a note that would not help.
    let red = core.state.items.get(item).filter(|it| {
        it.closed.is_none()
            && it.hold.is_none()
            && it.pending_fork.is_none()
            && it
                .pr_status
                .as_ref()
                .is_some_and(|s| s.phase == PrPhase::Open && s.checks == PrChecks::Red)
    });
    let Some(it) = red else {
        return Ok(());
    };
    // A note the item does not have is the fold's to refuse.
    if note.is_some_and(|n| Some(n) > it.red_since) {
        return Ok(());
    }
    Err(Error::Refused {
        why: "note_required".into(),
    })
}

/// What the tracker keeps per record offset, so a call does not scan the log.
#[derive(Default)]
struct Recall {
    /// The text of context, note, question, answer, decision and triage records.
    texts: HashMap<u64, Content>,
    /// The item each record names, so evidence can be held to its tree.
    subjects: HashMap<u64, ItemId>,
    /// Edges ingest made: its `linked` found no edge and the fold created it
    /// from that record. Ingest may unlink only these. The fold keeps no author
    /// for an edge, and one from a prerequisite, an answer or anyone else's
    /// link is never ingest's to remove.
    ingest_edges: HashSet<(ItemId, ItemId)>,
    /// The claims of each attempt and how each ended, so a terminal call
    /// retried after a lost answer can be told what it already wrote. A retry
    /// after a release or lapse claims the same attempt number again.
    attempts: HashMap<(ItemId, u32), Vec<ClaimLog>>,
    /// The attempt each `claimed` record is in, by its offset.
    claims: HashMap<u64, (ItemId, u32)>,
    /// The request keys callers sent lately, so a retry is answered with the first write.
    requests: request_keys::Requests,
}

/// One `claimed` record and the record that ended it.
struct ClaimLog {
    offset: u64,
    by: Key,
    ended: Option<Ended>,
}

/// The record that ended a claim.
struct Ended {
    offset: u64,
    kind: &'static str,
    /// What a `prerequisite` named, so a retry naming another is not replayed.
    prerequisite: Option<ItemId>,
    actor: Actor,
    by: Key,
}

impl Recall {
    /// `created` says the record made an edge that was not there before it.
    fn remember(
        &mut self,
        offset: u64,
        logged: &WorkLogged,
        me: &Key,
        created: bool,
        rejected: bool,
    ) {
        let record = &logged.record;
        if !rejected {
            self.requests.remember(offset, logged);
        }
        if let Some(item) = record.item() {
            self.subjects.insert(offset, item.clone());
        }
        if !rejected {
            self.attempt(offset, logged);
        }
        match record {
            WorkRecord::Linked { item, to } => {
                let edge = (item.clone(), to.clone());
                if logged.actor == Actor::Tracker && logged.by != *me {
                    if created {
                        self.ingest_edges.insert(edge);
                    }
                } else {
                    // A person or an agent who links it makes it theirs too.
                    self.ingest_edges.remove(&edge);
                }
            }
            WorkRecord::Unlinked { item, to } => {
                self.ingest_edges.remove(&(item.clone(), to.clone()));
            }
            // The waiter is `item` on the build and triage paths alike, and an
            // agent's wait (and its release ask) is not ingest's to drop.
            WorkRecord::Prerequisite {
                item, prerequisite, ..
            } => {
                self.ingest_edges
                    .remove(&(item.clone(), prerequisite.clone()));
            }
            _ => {}
        }
        remember(&mut self.texts, offset, record);
    }

    fn attempt(&mut self, offset: u64, logged: &WorkLogged) {
        let (item, attempt, ends) = match &logged.record {
            WorkRecord::Claimed { item, attempt, .. } => (item, *attempt, false),
            WorkRecord::Done { item, attempt, .. }
            | WorkRecord::Released { item, attempt, .. }
            | WorkRecord::GaveUp { item, attempt, .. } => (item, *attempt, true),
            // A triage prerequisite leaves the claim going on.
            WorkRecord::Prerequisite {
                item,
                attempt,
                triage: None,
                ..
            } => (item, *attempt, true),
            _ => return,
        };
        let claims = self.attempts.entry((item.clone(), attempt)).or_default();
        if !ends {
            self.claims.insert(offset, (item.clone(), attempt));
            claims.push(ClaimLog {
                offset,
                by: logged.by.clone(),
                ended: None,
            });
        } else if let Some(c) = claims.last_mut().filter(|c| c.ended.is_none()) {
            c.ended = Some(Ended {
                offset,
                kind: logged.record.kind(),
                prerequisite: match &logged.record {
                    WorkRecord::Prerequisite { prerequisite, .. } => Some(prerequisite.clone()),
                    _ => None,
                },
                actor: logged.actor,
                by: logged.by.clone(),
            });
        }
    }

    /// The offset of the newest `claimed` record of `attempt`.
    fn claimed(&self, item: &ItemId, attempt: u32) -> Option<u64> {
        let claims = self.attempts.get(&(item.clone(), attempt))?;
        claims.last().map(|c| c.offset)
    }

    /// The offset of the record that ended the claim `which` names, if `agent`
    /// itself wrote it as `kind`. By attempt, that is `agent`'s latest claim of
    /// it. A record the tracker or a person wrote, such as `released lapsed`, is
    /// not the agent's outcome.
    fn ended_by(
        &self,
        item: &ItemId,
        which: Which,
        agent: &AgentName,
        kind: &str,
        prerequisite: Option<&ItemId>,
    ) -> Option<u64> {
        let mine = |c: &&ClaimLog| c.by.as_str() == agent.as_str();
        let claim = match which {
            Which::Attempt(a) => self
                .attempts
                .get(&(item.clone(), a))?
                .iter()
                .rev()
                .find(mine)?,
            Which::Claim(at) => {
                let key = self.claims.get(&at).filter(|(i, _)| i == item)?;
                self.attempts.get(key)?.iter().find(|c| c.offset == at)?
            }
        };
        let ended = claim.ended.as_ref()?;
        let theirs = ended.kind == kind
            && ended.prerequisite.as_ref() == prerequisite
            && ended.actor == Actor::Agent
            && ended.by.as_str() == agent.as_str();
        theirs.then_some(ended.offset)
    }

    /// Whether the claim `agent` holds on `attempt` is not the one `which` names.
    fn other_claim(&self, item: &ItemId, attempt: u32, which: Option<Which>) -> bool {
        match which {
            None => false,
            Some(Which::Attempt(a)) => a != attempt,
            Some(Which::Claim(at)) => self.claimed(item, attempt) != Some(at),
        }
    }
}

/// Whether `record` is a `linked` whose edge `state` does not have.
fn edge_absent(state: &State, record: &WorkRecord) -> bool {
    matches!(record, WorkRecord::Linked { item, to } if !state.edges.contains(&(item.clone(), to.clone())))
}

/// Keeps the text of the records a read shows.
fn remember(texts: &mut HashMap<u64, Content>, offset: u64, record: &WorkRecord) {
    match record {
        WorkRecord::Context { text, .. }
        | WorkRecord::HumanInput { text, .. }
        | WorkRecord::Question { text, .. } => {
            texts.insert(offset, text.clone());
        }
        // An answer picks an option or says something, never both, so one entry reads back either.
        WorkRecord::Answer { text, chosen, .. } => {
            let text = match chosen {
                Some(c) => Content::Text(c.clone()),
                None => text.clone(),
            };
            texts.insert(offset, text);
        }
        // What the asker knew, which resume work reads as `so_far`.
        WorkRecord::Decision { so_far, .. } => {
            texts.insert(offset, Content::Text(so_far.clone()));
        }
        WorkRecord::Triaged { why, .. } => {
            texts.insert(offset, Content::Text(why.clone()));
        }
        WorkRecord::Prerequisite { new: Some(n), .. } => {
            texts.insert(offset, n.why.clone());
        }
        _ => {}
    }
}

/// What to do to Felix once the core lock is released.
enum Op {
    Ack(String, AgentName, u64),
    Nack(String, AgentName, GroupRecord),
}

type Slot = Arc<Mutex<Option<Delivery>>>;

/// Ends a hand-back mark if the settle task unwinds before it does, so a panic
/// cannot leave the item unclaimable until the tracker restarts.
struct HandBack<B: Broker>(Option<(Tracker<B>, ItemId)>);

impl<B: Broker> Drop for HandBack<B> {
    fn drop(&mut self) {
        // Drop cannot await the core lock, so the end runs on a task of its own.
        if let Some((me, item)) = self.0.take()
            && let Ok(rt) = tokio::runtime::Handle::try_current()
        {
            rt.spawn(async move { me.handed_back(&item).await });
        }
    }
}

struct Inner<B> {
    broker: B,
    core: Mutex<Core>,
    /// Taken after `core` when both are held.
    lease: StdMutex<LeaseState>,
    slots: StdMutex<HashMap<AgentName, Slot>>,
    /// One ready-record sync at a time.
    sync: Mutex<()>,
    /// One rebuild of the state at a time.
    reload: Mutex<()>,
    /// One renewal at a time: two renewals by the same version would refuse
    /// one of them, and the loser would fence a tracker that holds the lease.
    renewing: Mutex<()>,
    settings: Settings,
    me: Key,
    holder: String,
    stamp: AtomicU64,
    next_delivery: AtomicU64,
    /// Counts the hand-backs that have ended. See `Core::handed_back`.
    settles: AtomicU64,
    /// A round of `retry_recovering` is running.
    retrying: AtomicBool,
    /// Request keys whose write is in progress.
    running: StdMutex<HashSet<(request_keys::Caller, Key)>>,
}

pub(crate) struct Tracker<B>(Arc<Inner<B>>);

impl<B> Clone for Tracker<B> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<B: Broker> Tracker<B> {
    /// Takes the writer lease, rebuilds the state from the start of
    /// `work.events`, takes back the deliveries Felix still holds for agents the
    /// log shows holding, and publishes every ready item.
    pub(crate) async fn start(broker: B, settings: Settings) -> anyhow::Result<Self> {
        settings.check_review_tiers().map_err(anyhow::Error::msg)?;
        let tracker = Self(Arc::new(Inner {
            broker,
            core: Mutex::new(Core::default()),
            lease: StdMutex::default(),
            slots: StdMutex::default(),
            sync: Mutex::new(()),
            reload: Mutex::new(()),
            renewing: Mutex::new(()),
            settings,
            me: Key::new("tracker").expect("a valid key"),
            holder: holder_token()?,
            stamp: AtomicU64::new(now_micros()),
            next_delivery: AtomicU64::new(1),
            settles: AtomicU64::new(0),
            retrying: AtomicBool::new(false),
            running: StdMutex::default(),
        }));
        // Read once before taking the lease: a build that cannot read the log
        // must not hold the lease and keep a build that can from starting.
        tracker.read_state().await?;
        tracker.acquire_lease().await?;
        let (state, recall, tail) = tracker.read_state().await?;
        {
            let mut core = tracker.0.core.lock().await;
            core.state = state;
            core.recall = recall;
            core.tail = tail;
        }
        // Like a retake: the lease was granted before the read, so it is
        // confirmed by version before the first write.
        tracker.lease().refusals = 1;
        let holders: Vec<AgentName> = {
            let core = tracker.0.core.lock().await;
            core.state
                .items
                .values()
                .filter(|i| i.closed.is_none())
                .filter_map(|i| i.hold.as_ref())
                .filter_map(|h| AgentName::new(h.by.as_str()).ok())
                .collect()
        };
        for agent in holders {
            tracker.recover(&agent).await;
        }
        // A hold whose delivery was not found is gone: free the item.
        tracker.sweep().await;
        tracker.sync_ready().await;
        Ok(tracker)
    }

    /// Claims the highest-priority ready item for `agent`, waiting up to `wait`.
    #[cfg(test)]
    pub(crate) async fn next(&self, agent: &AgentName, wait: Duration) -> Result<Value, Error> {
        self.next_for(agent, wait, &[]).await
    }

    /// Like `next`, and the agent also takes reviews of the tiers it names.
    pub(crate) async fn next_for(
        &self,
        agent: &AgentName,
        wait: Duration,
        reviews: &[String],
    ) -> Result<Value, Error> {
        let lanes = self.review_lanes(reviews)?;
        let deadline = Instant::now() + wait.min(MAX_WAIT);
        loop {
            self.begin().await?;
            let slot = self.slot(agent);
            {
                let mut delivery = slot.lock().await;
                self.reconcile(agent, &mut delivery).await;
                self.resume_recover(agent, &mut delivery).await?;
                if let Some(d) = delivery.as_ref() {
                    let core = self.0.core.lock().await;
                    return Ok(claimed(&core, d, true));
                }
                let held = self.0.core.lock().await.state.held_by(agent.as_str());
                if let Some((item, _)) = held {
                    // The log says this agent holds an item, but the tracker has no
                    // delivery for it. The sweep frees the item; it is not handed
                    // back as a live claim.
                    return Ok(json!({
                        "claimed": false,
                        "note": format!("you have no current claim on {item}; it is being released"),
                        "loop": LOOP_RETRY,
                    }));
                }
                self.sync_ready().await;
                if let Some(d) = self.try_claim(agent, &lanes).await? {
                    let reply = claimed(&*self.0.core.lock().await, &d, false);
                    *delivery = Some(d);
                    return Ok(reply);
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(json!({ "claimed": false, "loop": LOOP_WHEN_EMPTY }));
            }
            tokio::time::sleep(left.min(POLL_EVERY)).await;
        }
    }

    /// Keeps the agent's delivery standing for another visibility period.
    pub(crate) async fn extend(&self, agent: &AgentName, item: &ItemId) -> Result<Value, Error> {
        self.begin().await?;
        let slot = self.slot(agent);
        let mut delivery = slot.lock().await;
        if self.reconcile(agent, &mut delivery).await.as_ref() == Some(item) {
            return Err(lapsed(item));
        }
        self.resume_recover(agent, &mut delivery).await?;
        let Some(d) = delivery.as_ref().filter(|d| &d.item == item) else {
            let core = self.0.core.lock().await;
            return Err(match core.state.items.get(item) {
                Some(i) if holds(i, agent) => restarted(item),
                _ => lapsed(item),
            });
        };
        let stream = ready_stream(d.tier);
        let mut extended = self.try_extend(&stream, agent, d).await;
        if matches!(extended, Err(ExtendError::Failed(_))) {
            // The connection may have dropped; a live agent keeps its claim on a new one.
            self.0.broker.forget(Some(agent.as_str())).await;
            extended = self.try_extend(&stream, agent, d).await;
        }
        match extended {
            Ok(visible) => {
                // The hold may have been released while Felix was answering.
                let mut core = self.0.core.lock().await;
                let ours = core.owner.get(item).is_some_and(|o| o.id == d.id);
                let shown = core
                    .state
                    .items
                    .get(item)
                    .and_then(|i| i.hold.as_ref())
                    .is_some_and(|h| h.by.as_str() == agent.as_str() && h.attempt == d.attempt);
                if !(ours && shown) {
                    if ours {
                        core.owner.remove(item);
                    }
                    drop(core);
                    delivery.take();
                    return Err(lapsed(item));
                }
                if let Some(o) = core.owner.get_mut(item) {
                    o.last_extended = Instant::now();
                }
                Ok(json!({
                    "item": item,
                    "deadline_ms": now_ms() + visible.as_millis() as u64,
                }))
            }
            Err(ExtendError::Stale) => {
                // Felix may have expired our lease early (its clock runs fast) and
                // let another tracker reclaim the delivery for the agent. Answer
                // `lost` only when this tracker still holds the lease; otherwise
                // the agent's retry reaches the tracker that does.
                if !self.confirm_lease().await {
                    return Err(Error::Unavailable(
                        "could not confirm the writer lease after Felix refused the extension"
                            .into(),
                    ));
                }
                // Felix says the delivery is gone, so there is nothing of ours to settle.
                let d = delivery.take().expect("checked above");
                let mut core = self.0.core.lock().await;
                if core.owner.get(item).is_some_and(|o| o.id == d.id) {
                    core.owner.remove(item);
                }
                Err(lapsed(item))
            }
            Err(ExtendError::Failed(e)) => Err(Error::Felix(e)),
        }
    }

    async fn try_extend(
        &self,
        stream: &str,
        agent: &AgentName,
        d: &Delivery,
    ) -> Result<Duration, ExtendError> {
        tokio::time::timeout(
            FELIX_TIMEOUT,
            self.0.broker.extend(
                stream,
                agent.as_str(),
                &d.record,
                self.0.settings.visibility,
            ),
        )
        .await
        .unwrap_or_else(|_| {
            Err(ExtendError::Failed(anyhow::anyhow!(
                "Felix did not answer in time"
            )))
        })
    }

    /// Writes `done`, then acks the delivery. `usage` is what the agent
    /// reports; the fold prices it and charges at least the cost floor.
    pub(crate) async fn complete(
        &self,
        agent: &AgentName,
        item: &ItemId,
        which: Option<Which>,
        usage: Vec<Usage>,
    ) -> Result<Value, Error> {
        self.finish(
            agent,
            item,
            which,
            "done",
            |attempt| WorkRecord::Done {
                item: item.clone(),
                attempt,
                cost_micros: 0,
                usage,
                floor_micros: self.floor(),
            },
            json!({ "item": item, "status": "done" }),
        )
        .await
    }

    /// Writes `released`, then hands the delivery back so another agent can take it.
    pub(crate) async fn release(
        &self,
        agent: &AgentName,
        item: &ItemId,
        which: Option<Which>,
        reason: Option<String>,
        usage: Vec<Usage>,
    ) -> Result<Value, Error> {
        self.finish(
            agent,
            item,
            which,
            "released",
            |attempt| WorkRecord::Released {
                item: item.clone(),
                attempt,
                reason,
                cost_micros: 0,
                usage,
                floor_micros: self.floor(),
            },
            json!({ "item": item, "status": "released" }),
        )
        .await
    }

    /// Writes `gave_up`. The item may be retried once, like a release.
    pub(crate) async fn give_up(
        &self,
        agent: &AgentName,
        item: &ItemId,
        which: Option<Which>,
        reason: String,
        usage: Vec<Usage>,
    ) -> Result<Value, Error> {
        self.finish(
            agent,
            item,
            which,
            "gave_up",
            |attempt| WorkRecord::GaveUp {
                item: item.clone(),
                attempt,
                reason,
                cost_micros: 0,
                usage,
                floor_micros: self.floor(),
            },
            json!({ "item": item, "status": "gave_up" }),
        )
        .await
    }

    /// The least a report is charged. It goes on the record, so every reader
    /// folds the same charge whatever the tracker is configured with later.
    pub(crate) fn floor(&self) -> u64 {
        self.0.settings.cost_floor_micros
    }

    /// Appends a record on behalf of a person, if the fold accepts it.
    pub(crate) async fn person_record(
        &self,
        person: Key,
        record: WorkRecord,
    ) -> Result<Value, Error> {
        tools::check_sizes(&record)?;
        self.begin().await?;
        let kind = record.kind();
        let offset = {
            let mut core = self.0.core.lock().await;
            self.append(&mut core, Actor::Person, person, record)
                .await?
        };
        self.sync_ready().await;
        Ok(json!({ "kind": kind, "offset": offset }))
    }

    /// Frees every hold whose delivery is gone, or that has not been extended
    /// for a visibility period, so the item is Ready without waiting for
    /// another agent to poll.
    pub(crate) async fn sweep(&self) {
        if self.begin().await.is_err() {
            return;
        }
        self.retry_recovering();
        let visibility = self.0.settings.visibility;
        // The lock is taken per hold, so one slow append does not keep it for the rest.
        let lapsed: Vec<ItemId> = {
            let core = self.0.core.lock().await;
            let now = Instant::now();
            core.state
                .items
                .values()
                .filter(|i| i.closed.is_none() && i.hold.is_some())
                .filter(|i| expired(&core, &i.id, now, visibility))
                .map(|i| i.id.clone())
                .collect()
        };
        for item in lapsed {
            let mut core = self.0.core.lock().await;
            // Checked again: the agent may have extended or finished meanwhile.
            let Some(attempt) = core
                .state
                .items
                .get(&item)
                .and_then(|i| i.hold.as_ref())
                .map(|h| h.attempt)
            else {
                continue;
            };
            if !expired(&core, &item, Instant::now(), visibility) {
                continue;
            }
            let released = WorkRecord::Released {
                item: item.clone(),
                attempt,
                reason: Some("lapsed".into()),
                // Letting a claim lapse must not be a way to work for free.
                cost_micros: 0,
                usage: Vec::new(),
                floor_micros: self.floor(),
            };
            if self
                .append(&mut core, Actor::Tracker, self.0.me.clone(), released)
                .await
                .is_ok()
            {
                // The delivery, if any, lapses in Felix by itself; no one settles it now.
                core.owner.remove(&item);
            }
        }
        self.sync_ready().await;
    }

    /// The items `agent` holds, by the log.
    pub(crate) async fn holds_of(&self, agent: &AgentName) -> Vec<ItemId> {
        let core = self.0.core.lock().await;
        core.state
            .items
            .values()
            .filter(|i| i.closed.is_none() && holds(i, agent))
            .map(|i| i.id.clone())
            .collect()
    }

    /// Releases every hold of a revoked agent, as the sweep releases a lapsed
    /// claim, and refuses its claims from now on. Answers the items released.
    pub(crate) async fn revoke(&self, agent: &AgentName) -> Result<Vec<ItemId>, Error> {
        self.0.core.lock().await.revoked.insert(agent.clone());
        self.begin().await?;
        let mut released = Vec::new();
        let mut core = self.0.core.lock().await;
        let held: Vec<(ItemId, u32)> = core
            .state
            .items
            .values()
            .filter(|i| i.closed.is_none() && holds(i, agent))
            .filter_map(|i| Some((i.id.clone(), i.hold.as_ref()?.attempt)))
            .collect();
        for (item, attempt) in held {
            let record = WorkRecord::Released {
                item: item.clone(),
                attempt,
                reason: Some("revoked".into()),
                cost_micros: 0,
                usage: Vec::new(),
                floor_micros: self.floor(),
            };
            self.append(&mut core, Actor::Tracker, self.0.me.clone(), record)
                .await?;
            // The delivery lapses in Felix by itself, as a lapsed claim's does.
            core.owner.remove(&item);
            released.push(item);
        }
        drop(core);
        self.sync_ready().await;
        Ok(released)
    }

    /// Renews the writer lease, sweeps, and closes idle agent connections, until
    /// the lease is lost for good.
    pub(crate) async fn maintain(&self) {
        // Renewal runs beside the sweep, not after it: a sweep's Felix calls can
        // take the full Felix timeout each, which would let the lease lapse.
        tokio::select! {
            () = self.keep_lease() => {}
            () = self.keep_swept() => {}
        }
    }

    /// Renews the lease every third of its time to live. Returns once another
    /// tracker has held it for [`FENCE_GRACE`].
    async fn keep_lease(&self) {
        let mut every = tokio::time::interval(self.0.settings.lease_ttl / 3);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut fenced_since: Option<Instant> = None;
        loop {
            // The first tick is at once: the teams start one after another, so
            // serving can begin late in this team's lease or after it ran out.
            every.tick().await;
            self.renew_lease().await;
            if self.lease().fenced {
                let since = *fenced_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= FENCE_GRACE {
                    eprintln!("kbl-tracker: another tracker holds the writer lease; exiting");
                    return;
                }
            } else {
                fenced_since = None;
            }
        }
    }

    async fn keep_swept(&self) {
        loop {
            self.sweep().await;
            self.0.broker.close_idle(MEMBER_IDLE).await;
            tokio::time::sleep(self.0.settings.lease_ttl / 3).await;
        }
    }

    // ---- the writer lease ----

    async fn acquire_lease(&self) -> anyhow::Result<()> {
        let deadline = Instant::now() + self.0.settings.lease_wait;
        loop {
            let started = Instant::now();
            let put = self
                .lease_io(self.0.broker.lease_put(
                    self.0.holder.as_bytes().to_vec(),
                    self.0.settings.lease_ttl,
                    None,
                ))
                .await;
            let held = match put {
                Ok(Some(version)) => Some((version, started + self.0.settings.lease_ttl)),
                // The put may have landed with its reply lost. When it is ours we
                // do not know when it was granted, so our view has run out and
                // the first renewal goes by version.
                put => match self.our_lease().await {
                    Some(version) => Some((version, Instant::now())),
                    None => {
                        put?;
                        None
                    }
                },
            };
            if let Some((version, valid_until)) = held {
                self.lease().held = Some(LeaseHeld {
                    version,
                    valid_until,
                });
                return Ok(());
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "another tracker holds the writer lease"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub(crate) async fn renew_lease(&self) {
        let seen = self.lease_until();
        let _one_at_a_time = self.0.renewing.lock().await;
        // A renewal that finished while this one waited already did the work.
        if self.lease_until() != seen {
            return;
        }
        let Some((version, valid_until, refusals)) = ({
            let lease = self.lease();
            lease
                .held
                .as_ref()
                .map(|l| (l.version, l.valid_until, lease.answerable()))
        }) else {
            return;
        };
        // Felix compares a version against the live entry. Our view of the lease
        // starts before the request, so Felix's entry outlives it by up to a
        // request's latency: renew by version first, even past our expiry, and
        // take the lease again with Absent only once Felix's entry is gone.
        let mut retaken = false;
        let started = Instant::now();
        let holder = self.0.holder.as_bytes().to_vec();
        let ttl = self.0.settings.lease_ttl;
        let mut put = self
            .lease_io(self.0.broker.lease_put(holder.clone(), ttl, Some(version)))
            .await;
        // A put of ours that landed with its reply lost left the entry at a
        // version we never heard of: adopt it and renew by it.
        let mut adopted = None;
        // A refusal we cannot check against the entry proves nothing: Felix may
        // be slow, and the entry our own late put left there.
        let mut unread = false;
        if !matches!(put, Ok(Some(_))) {
            let read = self.lease_read().await;
            unread = read.is_err();
            if let Some(ours) = read.ok().flatten().filter(|&v| v != version) {
                adopted = Some(ours);
            // Judged after the refusal: the put can land after our view ran out.
            // Absent cannot take a live entry, so trying it early is safe.
            } else if matches!(put, Ok(None)) && valid_until <= Instant::now() {
                retaken = true;
                put = self
                    .lease_io(self.0.broker.lease_put(holder.clone(), ttl, None))
                    .await;
                if !matches!(put, Ok(Some(_))) {
                    adopted = self.our_lease().await;
                }
            }
        }
        if let Some(ours) = adopted {
            // With our view run out the entry may come from an Absent put that
            // landed after another tracker held the lease: a retake.
            retaken |= valid_until <= Instant::now();
            put = self
                .lease_io(self.0.broker.lease_put(holder.clone(), ttl, Some(ours)))
                .await;
            // The entry we read can run out before this put lands. With our view
            // run out nobody may hold it now, so take it with Absent.
            if matches!(put, Ok(None)) && valid_until <= Instant::now() {
                retaken = true;
                put = self
                    .lease_io(self.0.broker.lease_put(holder, ttl, None))
                    .await;
            }
        }
        let mut lease = self.lease();
        match put {
            Ok(Some(version)) => {
                lease.held = Some(LeaseHeld {
                    version,
                    valid_until: started + ttl,
                });
                lease.fenced = false;
                lease.confirmed = lease.confirmed.max(refusals);
                // Another tracker may have held it and written meanwhile. Counted
                // as a refusal: the grant came before the rebuild reads, so a
                // renewal by version must confirm the lease after it.
                if retaken {
                    lease.stale = true;
                    lease.retaken = true;
                    lease.retakes += 1;
                    lease.refusals += 1;
                }
            }
            Ok(None) if unread && !retaken => {
                eprintln!(
                    "kbl-tracker: the writer lease renewal was refused and the entry could not be read"
                );
            }
            // Someone else holds it now.
            Ok(None) => {
                lease.held = None;
                lease.fenced = true;
            }
            // A blip: the lease stands until its own expiry passes.
            Err(e) => {
                eprintln!("kbl-tracker: could not renew the writer lease: {e:#}");
                if let Some(ours) = adopted
                    && lease.held.is_some()
                {
                    // Our view still bounds the entry: whatever landed was sent
                    // after it began.
                    lease.held = Some(LeaseHeld {
                        version: ours,
                        valid_until,
                    });
                    if retaken {
                        lease.stale = true;
                        lease.retaken = true;
                        lease.retakes += 1;
                        lease.refusals += 1;
                    }
                }
            }
        }
    }

    /// Whether a lease put sent after this call began says this tracker holds
    /// the lease, and holds it without a lapse another tracker could have used.
    async fn confirm_lease(&self) -> bool {
        let asked = Instant::now();
        let ttl = self.0.settings.lease_ttl;
        // Counted, not just flagged: another call's rebuild clears `retaken`.
        let retakes = self.lease().retakes;
        // A renewal already in flight was sent too early to count, so a second
        // one may be needed. One sent after `asked` by another caller counts.
        for _ in 0..2 {
            self.renew_lease().await;
            let lease = self.lease();
            if lease.fenced || lease.retaken || lease.retakes != retakes {
                return false;
            }
            if lease
                .held
                .as_ref()
                .is_some_and(|l| l.valid_until >= asked + ttl)
            {
                return true;
            }
        }
        false
    }

    /// The version of the lease entry when it holds this tracker's name. A
    /// failed read is no answer.
    async fn our_lease(&self) -> Option<u64> {
        self.lease_read().await.ok().flatten()
    }

    /// Like `our_lease`, but a failed read is an error rather than no answer.
    async fn lease_read(&self) -> anyhow::Result<Option<u64>> {
        Ok(match self.lease_io(self.0.broker.lease_get()).await? {
            Some((holder, version)) if holder == self.0.holder.as_bytes() => Some(version),
            _ => None,
        })
    }

    // ---- claims ----

    /// The lanes of the reviewer tiers an agent asks for. Reviews are claimed
    /// only by naming their tier.
    fn review_lanes(&self, reviews: &[String]) -> Result<Vec<u8>, Error> {
        if reviews.len() > usize::from(REVIEW_LANES) {
            return Err(Error::Invalid(format!(
                "at most {REVIEW_LANES} reviewer tiers"
            )));
        }
        let mut lanes = Vec::new();
        for name in reviews {
            let i = self
                .0
                .settings
                .review_tiers
                .iter()
                .position(|t| t == name)
                .ok_or_else(|| {
                    Error::Invalid(format!(
                        "{name:?} is not a reviewer tier here: use one of {}",
                        self.0.settings.review_tiers.join(", ")
                    ))
                })?;
            let lane = TIERS + u8::try_from(i).unwrap_or(0);
            if !lanes.contains(&lane) {
                lanes.push(lane);
            }
        }
        Ok(lanes)
    }

    /// Polls the priority tiers in order, then the reviewer lanes the agent
    /// named, and returns the first delivery that turns into a claim.
    async fn try_claim(&self, agent: &AgentName, review: &[u8]) -> Result<Option<Delivery>, Error> {
        for tier in (0..TIERS).chain(review.iter().copied()) {
            let stream = ready_stream(tier);
            // Nothing in this lane is for this agent: leave the records alone.
            if tier >= TIERS
                && (self.lane_is_all_own(agent, tier).await
                    || self.lane_is_emptied(agent, tier).await)
            {
                continue;
            }
            let mut passed = false;
            // Taken before the polls: an item published after the last empty poll
            // must not look like part of what the agent saw.
            let before = if tier >= TIERS {
                Some(self.lane_snapshot(agent, tier).await)
            } else {
                None
            };
            // A review of the agent's own work is for another reviewer. It is acked and
            // published again, not nacked: Felix counts every handout as an attempt and
            // dead-letters a record at its bound, so a nacking author would eat the review.
            let mut looked = 0;
            loop {
                // Stale and passed-over records are acked one by one; a lane full of
                // them must not hold the caller.
                looked += 1;
                if looked > MAX_LOOKED_AT {
                    break;
                }
                let polled_at = self.0.settles.load(Ordering::SeqCst);
                let polled = self
                    .io(
                        Some(agent.as_str()),
                        self.0
                            .broker
                            .poll(&stream, agent.as_str(), self.0.settings.visibility),
                    )
                    .await
                    .map_err(Error::Felix)?;
                let Some(record) = polled else {
                    if let (true, Some((at, others))) = (passed, before) {
                        self.mark_emptied(agent, tier, at, others).await;
                    }
                    break;
                };
                if tier >= TIERS && self.is_own_work(agent, &record).await {
                    passed = true;
                    // On its own task: a caller that gives up waiting (work_next's time
                    // is up) must not leave the record leased to this agent.
                    let me = self.clone();
                    let (agent, stream) = (agent.clone(), stream.clone());
                    tokio::spawn(async move { me.pass_over(&agent, &stream, record).await })
                        .await
                        .ok();
                    continue;
                }
                let mut ops = Vec::new();
                let taken = {
                    let mut core = self.0.core.lock().await;
                    self.claim_from(&mut core, agent, tier, record, polled_at, &mut ops)
                        .await
                };
                self.run(ops).await.ok();
                if let Some(d) = taken? {
                    return Ok(Some(d));
                }
            }
        }
        Ok(None)
    }

    /// Whether the lane has published Ready items and every one is a review of a
    /// tree this agent worked on. Items with no record in the lane (refused, or
    /// passed over and not yet published again) do not count.
    async fn lane_is_all_own(&self, agent: &AgentName, tier: u8) -> bool {
        let core = self.0.core.lock().await;
        let key = agent_key(agent);
        let mut any = false;
        for (id, it) in &core.state.items {
            let published = core.live.get(id).is_some_and(|(t, _)| *t == tier);
            if !published || core.state.stage(id) != Some(Stage::Ready) {
                continue;
            }
            any = true;
            if !core.state.worked_on(&it.root, &key) {
                return false;
            }
        }
        any
    }

    /// The published items in the lane that this agent has not worked on, in order.
    fn lane_others(core: &Core, agent: &AgentName, tier: u8) -> Vec<ItemId> {
        let key = agent_key(agent);
        let mut ids: Vec<ItemId> = core
            .live
            .iter()
            .filter(|(_, (t, _))| *t == tier)
            .filter(|(id, _)| {
                core.state
                    .items
                    .get(*id)
                    .is_none_or(|it| !core.state.worked_on(&it.root, &key))
            })
            .map(|(id, _)| (*id).clone())
            .collect();
        ids.sort();
        ids
    }

    /// The log position and the other agents' published items in the lane.
    async fn lane_snapshot(&self, agent: &AgentName, tier: u8) -> (Option<u64>, Vec<ItemId>) {
        let core = self.0.core.lock().await;
        (core.state.at, Self::lane_others(&core, agent, tier))
    }

    /// Remembers that the agent passed over its own reviews here and found nothing else.
    async fn mark_emptied(
        &self,
        agent: &AgentName,
        tier: u8,
        at: Option<u64>,
        others: Vec<ItemId>,
    ) {
        let mut core = self.0.core.lock().await;
        let mark = Emptied {
            at,
            others,
            until: Instant::now() + self.0.settings.visibility,
        };
        core.emptied.insert((agent.as_str().to_owned(), tier), mark);
    }

    /// Whether the lane is as the agent left it empty-handed.
    async fn lane_is_emptied(&self, agent: &AgentName, tier: u8) -> bool {
        let core = self.0.core.lock().await;
        core.emptied
            .get(&(agent.as_str().to_owned(), tier))
            .is_some_and(|m| {
                Instant::now() < m.until
                    && m.at == core.state.at
                    && m.others == Self::lane_others(&core, agent, tier)
            })
    }

    /// Whether the ready record is a review of a tree this agent worked on.
    async fn is_own_work(&self, agent: &AgentName, record: &GroupRecord) -> bool {
        let Ok(ready) = ReadyRecord::decode(&record.payload) else {
            return false;
        };
        let core = self.0.core.lock().await;
        let key = agent_key(agent);
        core.state
            .items
            .get(&ready.item)
            .is_some_and(|it| core.state.worked_on(&it.root, &key))
    }

    /// Acks a ready record this agent may not take and forgets that it was
    /// published, so a later sync publishes it again with a fresh stamp.
    async fn pass_over(&self, agent: &AgentName, stream: &str, record: GroupRecord) {
        if let Ok(ready) = ReadyRecord::decode(&record.payload) {
            let mut core = self.0.core.lock().await;
            // A newer record for the item stays live.
            if core.live.get(&ready.item) == Some(&(ready.tier, ready.stamp)) {
                core.live.remove(&ready.item);
            }
        }
        let ops = vec![Op::Ack(stream.to_owned(), agent.clone(), record.offset)];
        if let Err(e) = self.run(ops).await {
            eprintln!("kbl-tracker: could not ack a passed-over review: {e:#}");
        }
    }

    /// Turns one delivery into a claim, or settles it when it no longer
    /// describes claimable work.
    async fn claim_from(
        &self,
        core: &mut Core,
        agent: &AgentName,
        tier: u8,
        record: GroupRecord,
        polled_at: u64,
        ops: &mut Vec<Op>,
    ) -> Result<Option<Delivery>, Error> {
        let stream = ready_stream(tier);
        let ack =
            |ops: &mut Vec<Op>, offset| ops.push(Op::Ack(stream.clone(), agent.clone(), offset));
        let nack = |ops: &mut Vec<Op>, rec: &GroupRecord| {
            ops.push(Op::Nack(stream.clone(), agent.clone(), rec.clone()))
        };
        if core.revoked.contains(agent) {
            nack(ops, &record);
            return Err(Error::Refused {
                why: "agent_revoked".into(),
            });
        }
        let Ok(ready) = ReadyRecord::decode(&record.payload) else {
            eprintln!(
                "kbl-tracker: {stream} offset {} is not a ready record; acked",
                record.offset
            );
            ack(ops, record.offset);
            return Ok(None);
        };
        let item = ready.item.clone();
        // A newer record for the item (another tier, or published after a restart) wins.
        // No entry means this is the only record the tracker knows of, so it is adopted.
        match core.live.get(&item) {
            Some(&(t, s)) if (t, s) != (ready.tier, ready.stamp) => {
                ack(ops, record.offset);
                return Ok(None);
            }
            Some(_) => {}
            None => {
                core.live.insert(item.clone(), (ready.tier, ready.stamp));
            }
        }
        if ready.tier != tier {
            ack(ops, record.offset);
            return Ok(None);
        }
        // Polled before the last holder's nack ended, so that nack takes this
        // delivery too. It is no claim; the record comes round again.
        if core.handed_back.get(&item).is_some_and(|t| *t > polled_at) {
            return Ok(None);
        }
        match core.state.stage(&item) {
            Some(Stage::Ready) => {}
            Some(Stage::Held) => {
                let Some(hold) = core.state.items.get(&item).and_then(|i| i.hold.clone()) else {
                    return Ok(None);
                };
                // Felix handed the item out again, so the old claim lapsed.
                let lapse = WorkRecord::Released {
                    item: item.clone(),
                    attempt: hold.attempt,
                    reason: Some("lapsed".into()),
                    cost_micros: 0,
                    usage: Vec::new(),
                    floor_micros: self.floor(),
                };
                if let Err(e) = self
                    .append(core, Actor::Tracker, self.0.me.clone(), lapse)
                    .await
                {
                    nack(ops, &record);
                    return Err(e);
                }
            }
            _ => {
                core.live.remove(&item);
                ack(ops, record.offset);
                return Ok(None);
            }
        }
        if self.0.settings.crash_after_poll {
            std::process::exit(70);
        }
        let attempt = core.state.items[&item].next_attempt();
        let claim = WorkRecord::Claimed {
            item: item.clone(),
            attempt,
            deadline_ms: now_ms() + self.0.settings.visibility.as_millis() as u64,
        };
        match self
            .append(core, Actor::Agent, agent_key(agent), claim)
            .await
        {
            Ok(_) => {
                core.handed_back.remove(&item);
                core.published.remove(&item);
                let id = self.0.next_delivery.fetch_add(1, Ordering::Relaxed);
                // Whoever held the item before no longer owns its delivery, so
                // nothing it does later can settle this one.
                core.owner.insert(
                    item.clone(),
                    Owner {
                        agent: agent.clone(),
                        id,
                        last_extended: Instant::now(),
                    },
                );
                Ok(Some(Delivery {
                    id,
                    item,
                    attempt,
                    tier,
                    record,
                }))
            }
            Err(Error::Refused { why }) => {
                eprintln!("kbl-tracker: {item} not claimable ({why}); skipped");
                core.live.remove(&item);
                core.refused.insert(item.clone(), core.state.at);
                if why == "over_budget" {
                    self.log_over_budget(core, &item).await;
                }
                ack(ops, record.offset);
                Ok(None)
            }
            Err(e) => {
                nack(ops, &record);
                Err(e)
            }
        }
    }

    /// Writes the tracker's refusal so the root sits in Needs you and its ready
    /// items are not published again. A root already flagged is not logged twice.
    async fn log_over_budget(&self, core: &mut Core, item: &ItemId) {
        let flagged = core
            .state
            .items
            .get(item)
            .and_then(|i| core.state.items.get(&i.root))
            .is_none_or(|r| r.over_budget);
        if flagged {
            return;
        }
        let refusal = WorkRecord::Refused {
            item: Some(item.clone()),
            op: "claim".into(),
            reason: RefusalReason::Budget,
        };
        let _ = self
            .append(core, Actor::Tracker, self.0.me.clone(), refusal)
            .await;
    }

    /// Shared tail of complete, release and give_up: write the record the fold
    /// accepts, then settle the delivery. With `attempt`, a call whose claim
    /// already ended answers with the record that ended it, if this agent wrote
    /// it for that attempt as `kind`: the retry of a call whose answer was lost.
    async fn finish(
        &self,
        agent: &AgentName,
        item: &ItemId,
        which: Option<Which>,
        kind: &'static str,
        record: impl FnOnce(u32) -> WorkRecord,
        reply: Value,
    ) -> Result<Value, Error> {
        self.begin().await?;
        let slot = self.slot(agent);
        let mut delivery = slot.lock().await;
        if self.reconcile(agent, &mut delivery).await.as_ref() == Some(item) {
            let core = self.0.core.lock().await;
            self.recall_current()?;
            return replay(&core, agent, item, which, kind, reply).ok_or_else(|| lapsed(item));
        }
        let mut ops = Vec::new();
        let mut marked = false;
        let offset;
        {
            let mut core = self.0.core.lock().await;
            let held = match holding(&core, delivery.as_ref(), agent, item) {
                Ok(held) if core.recall.other_claim(item, held, which) => Err(lapsed(item)),
                held => held,
            };
            let held = match held {
                Ok(held) => held,
                Err(e) => {
                    self.recall_current()?;
                    return replay(&core, agent, item, which, kind, reply).ok_or(e);
                }
            };
            core.published.remove(item);
            offset = self
                .append(&mut core, Actor::Agent, agent_key(agent), record(held))
                .await?;
            let ready = core.state.stage(item) == Some(Stage::Ready);
            if !ready {
                core.live.remove(item);
            }
            if let Some(d) = delivery.take()
                && core.owner.get(item).is_some_and(|o| o.id == d.id)
            {
                core.owner.remove(item);
                let stream = ready_stream(d.tier);
                ops.push(if ready {
                    core.handed_back.insert(item.clone(), u64::MAX);
                    marked = true;
                    Op::Nack(stream, agent.clone(), d.record)
                } else {
                    Op::Ack(stream, agent.clone(), d.record.offset)
                });
            }
        }
        drop(delivery);
        if self.0.settings.crash_after_done {
            std::process::exit(70);
        }
        let settled = self.settle(item, ops, marked).await;
        let warning = settled
            .err()
            .map(|e| format!("the work is recorded but the delivery was not settled: {e:#}"));
        self.sync_ready().await;
        let mut reply = reply;
        reply["offset"] = offset.into();
        Ok(with_warning(reply, warning))
    }

    /// Called under `core` before a replay. A write whose reply was lost leaves
    /// the state showing its record but the claim index without it, until the
    /// rebuild; a retry that passed `begin` before then must not be told `lost`.
    fn recall_current(&self) -> Result<(), Error> {
        let lease = self.lease();
        if lease.stale || lease.retaken {
            return Err(Error::Unavailable("the state is being rebuilt".into()));
        }
        Ok(())
    }

    /// The fold is the truth for holds. If it no longer shows `agent` holding what
    /// it has a delivery for (a person closed it, an attempt was closed, another
    /// agent took it), the delivery is dropped. It is settled in Felix only when
    /// the tracker still stands behind that exact delivery for this agent.
    async fn reconcile(
        &self,
        agent: &AgentName,
        delivery: &mut Option<Delivery>,
    ) -> Option<ItemId> {
        let d = delivery.as_ref()?;
        let mut ops = Vec::new();
        let mut marked = false;
        {
            let mut core = self.0.core.lock().await;
            let ours = core
                .owner
                .get(&d.item)
                .is_some_and(|o| o.id == d.id && &o.agent == agent);
            let shown = core
                .state
                .items
                .get(&d.item)
                .and_then(|i| i.hold.as_ref())
                .is_some_and(|h| h.by.as_str() == agent.as_str() && h.attempt == d.attempt);
            if shown && ours {
                return None;
            }
            if ours {
                core.owner.remove(&d.item);
                let ready = core.state.stage(&d.item) == Some(Stage::Ready);
                if !ready {
                    core.live.remove(&d.item);
                }
                let stream = ready_stream(d.tier);
                ops.push(if ready {
                    core.handed_back.insert(d.item.clone(), u64::MAX);
                    marked = true;
                    Op::Nack(stream, agent.clone(), d.record.clone())
                } else {
                    Op::Ack(stream, agent.clone(), d.record.offset)
                });
            }
        }
        self.settle(&d.item, ops, marked).await.ok();
        delivery.take().map(|d| d.item)
    }

    /// Settles a finished holder's delivery, then ends the hand-back mark if
    /// this call set it (`marked`). It runs on its own task: a caller that goes
    /// away mid-nack must not leave the mark set, or the item is never claimed
    /// again. Only the call that set the mark ends it, so another caller cannot
    /// end it while the nack is still on its way.
    async fn settle(&self, item: &ItemId, ops: Vec<Op>, marked: bool) -> anyhow::Result<()> {
        if ops.is_empty() && !marked {
            return Ok(());
        }
        let (me, item) = (self.clone(), item.clone());
        tokio::spawn(async move {
            let mut mark = HandBack(marked.then(|| (me.clone(), item)));
            let settled = me.run(ops).await;
            if let Some((me, item)) = mark.0.take() {
                me.handed_back(&item).await;
            }
            settled
        })
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("the settle did not finish: {e}")))
    }

    /// Marks the hand-back of `item` as ended, whether or not the nack landed:
    /// a delivery polled from now on is a lease of its own. An item that left
    /// Ready meanwhile needs no mark.
    async fn handed_back(&self, item: &ItemId) {
        let mut core = self.0.core.lock().await;
        if core.state.stage(item) != Some(Stage::Ready) {
            core.handed_back.remove(item);
        } else if let Some(t) = core.handed_back.get_mut(item) {
            *t = self.0.settles.fetch_add(1, Ordering::SeqCst) + 1;
        }
    }

    /// Takes back the delivery Felix still holds for `agent`, whose first poll on
    /// its connection reclaims it. Any other delivery the agent had is handed back.
    async fn recover(&self, agent: &AgentName) {
        let slot = self.slot(agent);
        let mut delivery = slot.lock().await;
        self.recover_into(agent, &mut delivery).await;
    }

    /// `recover` with the agent's slot already locked. A poll that fails leaves
    /// the hold recovering, so the sweep keeps it until one visibility after the
    /// first failure; only an answer from Felix shows the delivery is gone.
    async fn recover_into(&self, agent: &AgentName, delivery: &mut Option<Delivery>) {
        let held = self.0.core.lock().await.state.held_by(agent.as_str());
        let Some((item, attempt)) = held.filter(|_| delivery.is_none()) else {
            self.0.core.lock().await.recovering.remove(agent.as_str());
            return;
        };
        // The broker reclaims in a connection's first poll, so that poll goes to
        // the tier the item was published on. Lanes an earlier try already
        // searched are skipped: polling them again would cost their head record
        // another attempt.
        let (first, mut searched) = {
            let core = self.0.core.lock().await;
            let first = core
                .state
                .items
                .get(&item)
                .and_then(|i| self.0.settings.lane(i))
                .unwrap_or(DEFAULT_TIER);
            let searched = core
                .recovering
                .get(agent.as_str())
                .map_or(0, |r| r.searched);
            (first, searched)
        };
        let mut others = Vec::new();
        let mut failed = false;
        let tiers: Vec<u8> = std::iter::once(first)
            .chain((0..LANES).filter(|t| *t != first))
            .filter(|t| searched & (1 << t) == 0)
            .collect();
        'tiers: for tier in tiers {
            let stream = ready_stream(tier);
            // Bounded, in case the broker keeps handing records back.
            for _ in 0..64 {
                let polled = self
                    .io(
                        Some(agent.as_str()),
                        self.0
                            .broker
                            .poll(&stream, agent.as_str(), self.0.settings.visibility),
                    )
                    .await;
                let record = match polled {
                    Ok(Some(record)) => record,
                    Ok(None) => break,
                    Err(_) => {
                        failed = true;
                        break 'tiers;
                    }
                };
                let ready = ReadyRecord::decode(&record.payload);
                let mut core = self.0.core.lock().await;
                if let Ok(ready) = &ready
                    && ready.item == item
                    // A revoke or a person may have ended the hold during the poll.
                    && core.state.held_by(agent.as_str()) == Some((item.clone(), attempt))
                {
                    let id = self.0.next_delivery.fetch_add(1, Ordering::Relaxed);
                    core.live.insert(item.clone(), (ready.tier, ready.stamp));
                    core.published.remove(&item);
                    core.owner.insert(
                        item.clone(),
                        Owner {
                            agent: agent.clone(),
                            id,
                            last_extended: Instant::now(),
                        },
                    );
                    *delivery = Some(Delivery {
                        id,
                        item: item.clone(),
                        attempt,
                        tier,
                        record,
                    });
                    break 'tiers;
                }
                // Felix hands back a member's own claims before new work, so a
                // record of the item or of a claimable one means nothing of ours
                // is left on this tier. Polling on would cost every Ready record
                // an attempt.
                let done = ready.as_ref().is_ok_and(|r| {
                    r.item == item || core.state.stage(&r.item) == Some(Stage::Ready)
                });
                drop(core);
                others.push(Op::Nack(stream.clone(), agent.clone(), record));
                if done {
                    break;
                }
            }
            searched |= 1 << tier;
        }
        {
            let mut core = self.0.core.lock().await;
            if failed && delivery.is_none() {
                let until = Instant::now() + self.0.settings.visibility;
                let r = core
                    .recovering
                    .entry(agent.as_str().to_owned())
                    .or_insert(Recovering { until, searched: 0 });
                r.searched = searched;
            } else {
                core.recovering.remove(agent.as_str());
            }
        }
        self.run(others).await.ok();
    }

    /// Starts `recover` again, on a task of its own, for every agent whose
    /// recover failed and whose hold has not run out. The sweep does not wait
    /// for it: a lane that hangs must not hold up the lease renewal behind the
    /// sweep. One round runs at a time.
    fn retry_recovering(&self) {
        if self.0.retrying.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let agents: Vec<AgentName> = {
                let mut core = me.0.core.lock().await;
                let now = Instant::now();
                core.recovering.retain(|_, r| now < r.until);
                core.recovering
                    .keys()
                    .filter_map(|a| AgentName::new(a.as_str()).ok())
                    .collect()
            };
            let mut tries = tokio::task::JoinSet::new();
            for agent in agents {
                let me = me.clone();
                tries.spawn(async move { me.recover(&agent).await });
            }
            while tries.join_next().await.is_some() {}
            me.0.retrying.store(false, Ordering::SeqCst);
        });
    }

    /// For an agent whose recover failed: tries again, and errs while Felix still
    /// fails, so the agent calls again instead of hearing its claim is lost.
    async fn resume_recover(
        &self,
        agent: &AgentName,
        delivery: &mut Option<Delivery>,
    ) -> Result<(), Error> {
        let pending = |core: &Core| {
            core.recovering
                .get(agent.as_str())
                .is_some_and(|r| Instant::now() < r.until)
        };
        if delivery.is_some() || !pending(&*self.0.core.lock().await) {
            return Ok(());
        }
        self.recover_into(agent, delivery).await;
        if delivery.is_none() && pending(&*self.0.core.lock().await) {
            return Err(Error::Felix(anyhow::anyhow!(
                "the tracker could not yet take back your claim after a restart; call again"
            )));
        }
        Ok(())
    }

    // ---- ready records ----

    /// Whether an item's ready record has stood so long that Felix has likely
    /// dead-lettered it. The tracker cannot see a dead letter, and a Ready item
    /// whose record is gone would never be claimed.
    fn record_lapsed(&self, core: &Core, id: &ItemId) -> bool {
        core.published.get(id).is_some_and(|(at, n)| {
            // Backs off with each republish, so an idle lane does not fill up.
            let periods = RECORD_LAPSE_PERIODS << (*n).min(4);
            at.elapsed() >= self.0.settings.visibility * periods
        })
    }

    /// Publishes a ready record for every Ready item that has none for its tier,
    /// or whose record has lapsed. The old record, if it survives, is stale by
    /// stamp and is acked when polled.
    async fn sync_ready(&self) {
        let _one_at_a_time = self.0.sync.lock().await;
        let want: Vec<Want> = {
            let mut core = self.0.core.lock().await;
            // The clock of a record runs only while its item is Ready: a long hold
            // does not make the record look old once the item is released.
            let ready: Vec<ItemId> = core
                .state
                .items
                .keys()
                .filter(|id| core.state.stage(id) == Some(Stage::Ready))
                .cloned()
                .collect();
            core.published.retain(|id, _| ready.iter().any(|r| r == id));
            // A hand-back still running keeps its mark; its own call ends it.
            core.handed_back
                .retain(|id, t| *t == u64::MAX || ready.iter().any(|r| r == id));
            for id in &ready {
                core.published
                    .entry(id.clone())
                    .or_insert((Instant::now(), 0));
            }
            core.state
                .items
                .iter()
                .filter(|(id, _)| core.state.stage(id) == Some(Stage::Ready))
                .filter(|(id, _)| core.refused.get(*id) != Some(&core.state.at))
                .filter_map(|(id, it)| Some((id.clone(), self.0.settings.lane(it)?)))
                .filter_map(|(id, tier)| {
                    let missing = core.live.get(&id).is_none_or(|(t, _)| *t != tier);
                    let lapsed = !missing && self.record_lapsed(&core, &id);
                    (missing || lapsed).then(|| {
                        let prior = core.live.get(&id).copied();
                        (id, tier, prior, lapsed)
                    })
                })
                .collect()
        };
        for (item, tier, prior, lapsed) in want {
            let stamp = self.0.stamp.fetch_add(1, Ordering::Relaxed);
            let record = ReadyRecord {
                item: item.clone(),
                tier,
                stamp,
            };
            match self
                .io(
                    None,
                    self.0.broker.publish(&ready_stream(tier), record.encode()),
                )
                .await
            {
                Ok(_) => {
                    let mut core = self.0.core.lock().await;
                    // A claim or another sync may have landed while the lock was off.
                    // The new record is then left unrecorded and acked as stale when
                    // it is delivered; recording it would name a Held item's record.
                    if core.state.stage(&item) == Some(Stage::Ready)
                        && core.live.get(&item).copied() == prior
                    {
                        let n = core.published.get(&item).map_or(0, |(_, n)| *n);
                        core.published.insert(
                            item.clone(),
                            (Instant::now(), if lapsed { n + 1 } else { 0 }),
                        );
                        core.live.insert(item, (tier, stamp));
                    }
                }
                // The next sync tries again.
                Err(e) => eprintln!("kbl-tracker: could not publish {item} as ready: {e:#}"),
            }
        }
    }

    // ---- the log ----

    /// Appends `record` if the fold accepts it, and applies it to the live state.
    async fn append(
        &self,
        core: &mut Core,
        actor: Actor,
        by: Key,
        mut record: WorkRecord,
    ) -> Result<u64, Error> {
        {
            let writer = self.lease();
            if let Some(why) = unavailable(&writer, self.lease_margin()) {
                return Err(Error::Unavailable(why));
            }
            // A retake after begin() rebuilt: the state may miss another tracker's records.
            // Stale after a refused or failed write: the state holds a record the
            // log may not, so an append on it could land on another writer's record.
            if writer.retaken || writer.stale {
                return Err(Error::Unavailable("the state is being rebuilt".into()));
            }
            // A caller past begin() can get here after another call's rebuild and
            // before its lease check, while another tracker may hold the lease.
            if writer.refusals > writer.confirmed {
                return Err(Error::Unavailable(
                    "checking the writer lease after a refused write".into(),
                ));
            }
        }
        // A kind this build does not know decodes to `unknown`. Logging it
        // would put a record on the log that says nothing.
        if record == WorkRecord::Unknown {
            return Err(Error::Invalid("unknown kind of record".into()));
        }
        // Every route (agent, person, ingest) ends here, so one check bounds them all.
        tools::check_sizes(&record)?;
        canonical_pr(&core.state, &mut record);
        // A log below format 8 cannot carry usage: its report is charged the floor.
        record.fit_charge(core.state.format());
        if actor == Actor::Agent {
            kbl_core::redact::redact_record(&mut record);
        }
        let req = (actor != Actor::Tracker)
            .then(request_keys::current)
            .flatten();
        let logged = WorkLogged::new(now_ms(), by, actor, record).with_request(req);
        // The tracker is the only writer, so the next offset is the one after the
        // last applied, or the read tip when a generation start moved it.
        let guess = core.state.at.map_or(0, |a| a + 1).max(core.tail);
        // Only for a writer the fold lets act, so no one else learns which tree a record is in.
        if core.state.may_cite(&logged).is_ok() {
            evidence_in_tree(core, &logged.record, guess)?;
        }
        retry_has_note(core, &logged.record)?;
        let absent = edge_absent(&core.state, &logged.record);
        // Applied before the write: a refusal changes nothing, and a failed write
        // marks the state stale so the next call rebuilds it from the log.
        if let Err(why) = core.state.try_apply(guess, &logged) {
            let why = serde_json::to_value(why)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| "invalid".into());
            return Err(Error::Refused { why });
        }
        // Conditional on the tail: if another tracker took the lease and wrote,
        // Felix refuses this, whatever this tracker's clock says.
        match self
            .io(
                None,
                self.0
                    .broker
                    .publish_if(WORK_EVENTS, logged.encode(), guess),
            )
            .await
        {
            Ok(ConditionalWrite::Written { offset }) => {
                core.tail = offset + 1;
                core.recall
                    .remember(offset, &logged, &self.0.me, absent, false);
                if logged.req.is_some() {
                    request_keys::landed();
                }
                Ok(offset)
            }
            // The record was applied above, so the state is rebuilt, and the
            // lease checked with Felix, before the next write.
            Ok(ConditionalWrite::Refused { .. }) => {
                self.lease().refused();
                Err(Error::Unavailable(format!(
                    "{WORK_EVENTS} moved under this write; try again"
                )))
            }
            // A lost reply may have been a refusal: check the lease too.
            Err(e) => {
                self.lease().refused();
                Err(Error::Felix(e))
            }
        }
    }

    /// Refuses to serve without the writer lease, and rebuilds the state first
    /// if it may not match the log.
    async fn begin(&self) -> Result<(), Error> {
        // Renew first when the lease is too close to running out for a write to
        // land. While the state is stale this does not confirm a refused write.
        let renew = {
            let lease = self.lease();
            !lease.fenced
                && lease
                    .held
                    .as_ref()
                    .is_some_and(|l| l.valid_until <= Instant::now() + self.lease_margin())
        };
        if renew {
            self.renew_lease().await;
        }
        let (stale, refused) = {
            let lease = self.lease();
            if let Some(why) = unavailable(&lease, self.lease_margin()) {
                return Err(Error::Unavailable(why));
            }
            (lease.stale, lease.refusals > lease.confirmed)
        };
        if stale {
            self.rebuild().await?;
        }
        // Felix refused a write, so another tracker may hold the lease. Checked
        // after the rebuild: a grant then means no one else wrote since the read.
        if refused {
            self.renew_lease().await;
            let lease = self.lease();
            if let Some(why) = unavailable(&lease, self.lease_margin()) {
                return Err(Error::Unavailable(why));
            }
            if lease.refusals > lease.confirmed {
                return Err(Error::Unavailable(
                    "checking the writer lease after a refused write".into(),
                ));
            }
        }
        Ok(())
    }

    /// Rebuilds the state from the log, unless another call just did.
    async fn rebuild(&self) -> Result<(), Error> {
        let _one_at_a_time = self.0.reload.lock().await;
        let retakes = {
            // Taken for its wait: an append holds `core` over its write, so the
            // read below starts after any write already under way.
            let _core = self.0.core.lock().await;
            let lease = self.lease();
            if !lease.stale {
                return Ok(());
            }
            lease.retakes
        };
        // Built beside the live state and swapped in on success, so a failed read
        // leaves the tracker refusing to serve rather than serving nothing.
        let (state, recall, tail) = self
            .read_state()
            .await
            .map_err(|e| Error::Unavailable(format!("rebuilding from the log failed: {e:#}")))?;
        let mut core = self.0.core.lock().await;
        core.state = state;
        core.recall = recall;
        // Assigned, not raised: a shard failover can lower the tip.
        core.tail = tail;
        // A retake during the read may have let records in that it missed.
        let mut lease = self.lease();
        if lease.retakes == retakes {
            lease.stale = false;
            lease.retaken = false;
        }
        Ok(())
    }

    /// The fold of every record in `work.events`, and the committed tip.
    async fn read_state(&self) -> anyhow::Result<(State, Recall, u64)> {
        let mut entries = Vec::new();
        let (records, tail) = self
            .io(None, self.0.broker.read_to_tip(WORK_EVENTS, 0))
            .await?;
        for (offset, payload) in records {
            match read(&payload) {
                Ok(rec) => entries.push((offset, rec)),
                // Folding past a record of a newer format would give a state its
                // writer did not mean, so the tracker does not serve at all.
                Err(e @ ReadError::Newer { .. }) => {
                    anyhow::bail!("{WORK_EVENTS} offset {offset}: {e}")
                }
                Err(e) => eprintln!("kbl-tracker: {WORK_EVENTS} offset {offset}: {e}; skipped"),
            }
        }
        let mut recall = Recall::default();
        let mut state = State::default();
        for (offset, rec) in &entries {
            let absent = edge_absent(&state, &rec.record);
            state.apply(*offset, rec);
            let created = absent && !edge_absent(&state, &rec.record);
            // The append path remembers only what the fold took; so does a rebuild.
            let rejected = state.rejections.last().is_some_and(|r| r.offset == *offset);
            recall.remember(*offset, rec, &self.0.me, created, rejected);
        }
        Ok((state, recall, tail))
    }

    fn slot(&self, agent: &AgentName) -> Slot {
        self.0
            .slots
            .lock()
            .expect("slots lock")
            .entry(agent.clone())
            .or_default()
            .clone()
    }

    /// A Felix call with a time limit. A failure or a timeout means the
    /// connection it used may be dead, so it is dropped and the next call opens
    /// a new one. `member` names an agent's connection; none is the log's.
    async fn io<T>(
        &self,
        member: Option<&str>,
        call: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        self.io_within(FELIX_TIMEOUT, member, call).await
    }

    /// A call on the lease key, with the lease's shorter time limit.
    async fn lease_io<T>(
        &self,
        call: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        self.io_within(LEASE_TIMEOUT, None, call).await
    }

    async fn io_within<T>(
        &self,
        limit: Duration,
        member: Option<&str>,
        call: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        let done = tokio::time::timeout(limit, call)
            .await
            .map_err(|_| anyhow::anyhow!("Felix did not answer in time"))
            .and_then(|r| r);
        if done.is_err() {
            self.0.broker.forget(member).await;
        }
        done
    }

    /// Runs the Felix settles decided under the core lock. A settle that fails
    /// is tried once more on a new connection. Failures are logged; the first
    /// is returned.
    async fn run(&self, ops: Vec<Op>) -> anyhow::Result<()> {
        let mut first = Ok(());
        for op in ops {
            let mut done = Ok(());
            for _ in 0..2 {
                done = match &op {
                    Op::Ack(stream, agent, offset) => {
                        self.io(
                            Some(agent.as_str()),
                            self.0.broker.ack(stream, agent.as_str(), *offset),
                        )
                        .await
                    }
                    Op::Nack(stream, agent, record) => {
                        self.io(
                            Some(agent.as_str()),
                            self.0.broker.nack(stream, agent.as_str(), record),
                        )
                        .await
                    }
                };
                if done.is_ok() {
                    break;
                }
            }
            if let Err(e) = done {
                eprintln!("kbl-tracker: settling a delivery failed: {e:#}");
                if first.is_ok() {
                    first = Err(e);
                }
            }
        }
        first
    }

    fn lease_until(&self) -> Option<Instant> {
        self.lease().held.as_ref().map(|l| l.valid_until)
    }

    fn lease(&self) -> std::sync::MutexGuard<'_, LeaseState> {
        self.0.lease.lock().expect("lease lock")
    }

    /// How long before the lease runs out the tracker stops writing. Capped at
    /// half the lease, so a write can still take longer than what is left (7.5 s
    /// against a 10 s Felix timeout by default). What makes a late write safe is
    /// that every append is conditional on the tail: Felix refuses it once
    /// another tracker has written.
    fn lease_margin(&self) -> Duration {
        (FELIX_TIMEOUT + LEASE_SKEW).min(self.0.settings.lease_ttl / 2)
    }
}

/// Whether the hold on `item` has no delivery behind it, or was last extended
/// a visibility period ago.
fn expired(core: &Core, item: &ItemId, now: Instant, visibility: Duration) -> bool {
    let holder = core.state.items.get(item).and_then(|i| i.hold.as_ref());
    if holder.is_some_and(|h| {
        core.recovering
            .get(h.by.as_str())
            .is_some_and(|r| now < r.until)
    }) {
        return false;
    }
    core.owner
        .get(item)
        .is_none_or(|o| now.duration_since(o.last_extended) >= visibility)
}

/// Why the tracker cannot serve, if it cannot.
fn unavailable(lease: &LeaseState, margin: Duration) -> Option<String> {
    if lease.fenced {
        return Some("another tracker holds the writer lease".into());
    }
    match &lease.held {
        Some(l) if l.valid_until > Instant::now() + margin => None,
        _ => Some("this tracker does not hold the writer lease".into()),
    }
}

fn holds(item: &kbl_core::work_fold::Item, agent: &AgentName) -> bool {
    item.hold
        .as_ref()
        .is_some_and(|h| h.by.as_str() == agent.as_str())
}

/// The attempt `agent` holds `item` on: from its delivery, or from the log when
/// the tracker has no delivery for it.
fn holding(
    core: &Core,
    delivery: Option<&Delivery>,
    agent: &AgentName,
    item: &ItemId,
) -> Result<u32, Error> {
    if let Some(d) = delivery {
        return if &d.item == item {
            Ok(d.attempt)
        } else {
            Err(Error::NotHeld(format!("you hold {}, not {item}", d.item)))
        };
    }
    match core.state.items.get(item) {
        Some(i) if holds(i, agent) => Ok(i.hold.as_ref().map_or(1, |h| h.attempt)),
        // Someone else holds it or it is closed: the claim was lost.
        Some(i) if i.hold.is_some() || i.closed.is_some() => Err(lapsed(item)),
        _ => Err(Error::NotHeld(format!("you do not hold {item}"))),
    }
}

/// The answer to a terminal call retried after its record landed: `reply`
/// with the offset of the record that ended `attempt`, when `agent` wrote it as
/// `kind`. Without an attempt nothing is replayed, as for an older `kbl-mcp`.
fn replay(
    core: &Core,
    agent: &AgentName,
    item: &ItemId,
    which: Option<Which>,
    kind: &str,
    mut reply: Value,
) -> Option<Value> {
    // A prerequisite's answer names what it waits on; the retry must name the same.
    let prerequisite = reply["prerequisite"]
        .as_str()
        .and_then(|p| ItemId::new(p).ok());
    let offset = core
        .recall
        .ended_by(item, which?, agent, kind, prerequisite.as_ref())?;
    reply["offset"] = offset.into();
    reply["replayed"] = true.into();
    Some(reply)
}

fn lapsed(item: &ItemId) -> Error {
    Error::Lost(format!(
        "your claim on {item} lapsed and may have gone to another agent. Stop working on it; use work_next for new work"
    ))
}

fn restarted(item: &ItemId) -> Error {
    Error::Lost(format!(
        "the tracker has no delivery for your claim on {item}, so it cannot extend it. It will be released; use work_next for new work"
    ))
}

fn claimed(core: &Core, delivery: &Delivery, resumed: bool) -> Value {
    let state = &core.state;
    let item = state.items.get(&delivery.item);
    // A review reads its root's request. A fix a review asked for lands on that
    // request's branch, so its author goes on from there.
    let owner = item
        .filter(|i| i.review.is_some() || i.finding.is_some())
        .and_then(|i| i.parent.as_ref())
        .and_then(|p| state.items.get(p));
    let pr_branch = match owner {
        Some(root) => kbl_core::work::pr_branch(&root.id),
        None => kbl_core::work::pr_branch(&delivery.item),
    };
    json!({
        "claimed": true,
        "resumed": resumed,
        "attempt": delivery.attempt,
        // The `claimed` record's offset.
        "offset": core.recall.claimed(&delivery.item, delivery.attempt),
        "item": {
            "id": delivery.item,
            "title": item.map(|i| i.title.as_str()),
            "work": item.map(|i| i.work),
            "tier": item.and_then(|i| i.tier),
            "parent": item.and_then(|i| i.parent.as_ref()),
            "root": item.map(|i| &i.root),
            "branch": item.and_then(|i| i.branch.as_ref()),
            // The head branch to open the pull request from; it links the request to this item.
            "pr_branch": pr_branch,
            "pr": owner.or(item).and_then(|i| i.pr.as_ref()),
            "review": item.and_then(|i| i.review.as_ref()),
            "finding": item.and_then(|i| i.finding),
            "context": item.map(|i| i.context.keys().collect::<Vec<_>>()),
        },
        "deadline_ms": item.and_then(|i| i.hold.as_ref()).map(|h| h.deadline_ms),
        // Read-only for the agent: only a person's `rules_set` changes it.
        "rules": state.rules_for(&delivery.item),
        "loop": LOOP_AFTER_CLAIM,
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

mod tools;

pub(crate) use tools::{Decide, NewFinding, Post, Prereq, Reply, TriageAnswer};

#[cfg(test)]
mod tests;
