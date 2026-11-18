//! The org writer: the fold of `org.events` and the one place that appends to
//! it. It holds a lease of its own, the `writer` key in `org.tracker`, so a
//! second tracker cannot write the org log while this one does. See "One
//! tracker, many teams" in docs/design.md.

use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use kbl_core::codec::Codec;
use kbl_core::names::Key;
use kbl_core::org::{ORG_EVENTS, OrgLogged, OrgReadError, OrgRecord, TeamName, read};
use kbl_core::org_fold::{Applied, OrgState, Principal};
use tokio::sync::{Mutex, watch};
use tokio::time::Instant;

use crate::felix::{Broker, ConditionalWrite, holder_token};

mod backoff;

/// How long the org lease may stay lost before the process exits. Past it,
/// revocations would stop taking effect.
pub(crate) const ORG_LEASE_GRACE: Duration = Duration::from_secs(60);

/// The longest one Felix call may take. A write must land while the lease
/// still stands, so the lease must outlast it plus some clock skew.
const FELIX_TIMEOUT: Duration = Duration::from_secs(10);
const LEASE_SKEW: Duration = Duration::from_secs(1);

/// How many times an org write reads the log again after Felix refused it.
const ORG_WRITE_TRIES: usize = 3;

/// A Felix call with a time limit, so a hung call cannot outlast the lease or
/// hold the org lock for good.
async fn timed<T>(call: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(FELIX_TIMEOUT, call)
        .await
        .map_err(|_| anyhow::anyhow!("Felix did not answer in time"))?
}

/// Why an org change was not made.
#[derive(Debug)]
pub(crate) enum OrgError {
    /// The fold would not take the record. `why` is its code.
    Refused(&'static str),
    /// The org lease is not held, or the log could not be read.
    Unavailable(String),
    /// Felix failed. The record may or may not be on the log.
    Felix(anyhow::Error),
}

struct Lease {
    version: u64,
    valid_until: Instant,
}

struct Core {
    state: OrgState,
    /// The offset of the last record read or written.
    at: Option<u64>,
    lease: Option<Lease>,
    /// The state may not match the log; read it again before the next write.
    stale: bool,
    /// The offset after the last committed record read, skipped offsets
    /// included (a new shard leader's generation start). Never taken from a
    /// refusal, whose tail counts offsets claimed but not yet committed.
    tail: u64,
    /// Counts conditional writes Felix refused. One past `confirmed` means
    /// another tracker may hold the lease: no write until a renewal says not.
    refusals: u64,
    /// `refusals` as of the last renewal Felix granted by version.
    confirmed: u64,
}

impl Core {
    /// The refusals a lease put sent now answers for: only those whose state
    /// was read again before it. A grant before the read says nothing about
    /// records that land during it.
    fn answerable(&self) -> u64 {
        if self.stale {
            self.confirmed
        } else {
            self.refusals
        }
    }

    /// Where the next record must land for the log to be the one folded.
    fn next(&self) -> u64 {
        self.at.map_or(0, |a| a + 1).max(self.tail)
    }
}

struct View {
    state: OrgState,
    /// Until when `state` can be trusted: the lease is held and the log was
    /// read since it was taken. Unset, another tracker may have revoked a
    /// token this state still knows.
    held_until: Option<Instant>,
}

/// The org view cannot answer for a token: this tracker does not hold the org
/// lease, or has not read the log since taking it back.
#[derive(Debug)]
pub(crate) struct OrgUnavailable;

pub(crate) struct Org<B> {
    broker: B,
    core: Mutex<Core>,
    /// The state as last folded, for the token check. The core lock is held
    /// across Felix writes, and a call must not wait on one to be let in.
    view: RwLock<View>,
    lease_ttl: Duration,
    holder: String,
    /// The offset after the last record folded into the view, so a long poll
    /// wakes when the org changes.
    changed: watch::Sender<u64>,
}

impl<B: Broker> Org<B> {
    /// Takes the org lease, waiting up to `lease_wait` for another holder's
    /// to run out, and folds `org.events`.
    pub(crate) async fn start(
        broker: B,
        lease_ttl: Duration,
        lease_wait: Duration,
    ) -> Result<Self> {
        let org = Self {
            broker,
            core: Mutex::new(Core {
                state: OrgState::default(),
                at: None,
                lease: None,
                stale: true,
                tail: 0,
                refusals: 0,
                confirmed: 0,
            }),
            view: RwLock::new(View {
                state: OrgState::default(),
                held_until: None,
            }),
            lease_ttl,
            holder: holder_token()?,
            changed: watch::Sender::new(0),
        };
        // Read before taking the lease: a build that cannot read the org log
        // must not hold the lease and keep one that can from starting.
        org.read().await?;
        let deadline = Instant::now() + lease_wait;
        loop {
            if org.take_lease().await? {
                break;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "another tracker holds the org writer lease"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let mut core = org.core.lock().await;
        if core.stale {
            org.refresh(&mut core).await?;
        }
        drop(core);
        Ok(org)
    }

    /// The org as last folded.
    pub(crate) async fn state(&self) -> OrgState {
        self.core.lock().await.state.clone()
    }

    /// Whose token it is, by the last fold. A revoked agent's token names no
    /// one. Refused while the lease does not back the fold.
    pub(crate) fn who(&self, token: &str) -> Result<Option<Principal>, OrgUnavailable> {
        let view = self.view.read().expect("org view lock");
        if !view
            .held_until
            .is_some_and(|t| t > Instant::now() + LEASE_SKEW)
        {
            return Err(OrgUnavailable);
        }
        Ok(view.state.who(token))
    }

    /// Whether `person` is in `team` by the last fold. An org admin is in every team.
    pub(crate) fn is_member(&self, team: &TeamName, person: &Key) -> bool {
        let view = self.view.read().expect("org view lock");
        view.state.role_in(team, person).is_some()
    }

    /// Whether `agent` is an agent of `team` that is not revoked, by the last fold.
    pub(crate) fn has_agent(&self, team: &TeamName, agent: &Key) -> bool {
        let view = self.view.read().expect("org view lock");
        view.state.agent(team, agent).is_some_and(|a| !a.revoked)
    }

    /// Whether the lease is held with room for a write to land, and the log was
    /// read since it was taken. False is a hint, not a refusal: a write after a
    /// failed read may still get in by reading first.
    pub(crate) fn writable(&self) -> bool {
        let view = self.view.read().expect("org view lock");
        view.held_until
            .is_some_and(|t| t > Instant::now() + self.write_margin())
    }

    /// How long the lease must still stand for a write to be let in.
    fn write_margin(&self) -> Duration {
        (FELIX_TIMEOUT + LEASE_SKEW).min(self.lease_ttl / 2)
    }

    /// Whether a write sent now lands while both the lease that let the call
    /// in and the current one still stand.
    fn has_room(&self, core: &Core, let_in_until: Option<Instant>) -> bool {
        let by = Instant::now() + self.write_margin();
        let_in_until.is_some_and(|t| t > by)
            && core.lease.as_ref().is_some_and(|l| l.valid_until > by)
    }

    fn publish_view(&self, core: &Core) {
        let mut view = self.view.write().expect("org view lock");
        view.state = core.state.clone();
        view.held_until = Self::held_until(core);
        drop(view);
        let next = core.at.map_or(0, |a| a + 1);
        self.changed
            .send_if_modified(|at| std::mem::replace(at, next) != next);
    }

    /// Answers from the org as last folded, with the offset after its last
    /// record. Reads the view, so it never waits behind a Felix write. None
    /// while the lease does not back the view, as [`Self::who`].
    pub(crate) fn with_view<T>(&self, f: impl FnOnce(&OrgState, u64) -> T) -> Option<T> {
        let view = self.view.read().expect("org view lock");
        if !view
            .held_until
            .is_some_and(|t| t > Instant::now() + LEASE_SKEW)
        {
            return None;
        }
        Some(f(&view.state, *self.changed.borrow()))
    }

    /// Wakes when the org view moves past what the receiver has seen.
    pub(crate) fn watch(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn publish_lease(&self, core: &Core) {
        self.view.write().expect("org view lock").held_until = Self::held_until(core);
    }

    fn held_until(core: &Core) -> Option<Instant> {
        if core.stale {
            return None;
        }
        core.lease.as_ref().map(|l| l.valid_until)
    }

    /// Appends `record` if the fold takes it and it changes something. The
    /// answer says whether it changed anything, and where it landed.
    pub(crate) async fn append(
        &self,
        by: Key,
        record: OrgRecord,
    ) -> Result<(Applied, Option<u64>), OrgError> {
        let mut core = self.core.lock().await;
        // Every write must land while the lease that let this call in still
        // stands. That also bounds how long the core lock keeps renew() out.
        let let_in_until = core.lease.as_ref().map(|l| l.valid_until);
        if !self.has_room(&core, let_in_until) {
            return Err(OrgError::Unavailable(
                "this tracker does not hold the org writer lease".into(),
            ));
        }
        // Read first, then check the lease: Felix granting the version after
        // the read means no other tracker wrote since.
        if core.stale {
            self.refresh(&mut core)
                .await
                .map_err(|e| OrgError::Unavailable(format!("reading {ORG_EVENTS}: {e:#}")))?;
        }
        if core.refusals > core.confirmed {
            self.confirm_lease(&mut core).await?;
        }
        // Conditional on the tail: if another tracker took the lease and wrote,
        // Felix refuses this, whatever this tracker's clock says.
        for _ in 0..ORG_WRITE_TRIES {
            let logged = OrgLogged::new(now_ms(), by.clone(), record.clone());
            match core.state.check(&logged) {
                Err(why) => return Err(OrgError::Refused(why.why())),
                Ok(Applied::Unchanged) => return Ok((Applied::Unchanged, None)),
                Ok(Applied::Changed) => {}
            }
            if !self.has_room(&core, let_in_until) {
                return Err(OrgError::Unavailable(
                    "the org writer lease ran short during this write; try again".into(),
                ));
            }
            let expected = core.next();
            match timed(
                self.broker
                    .publish_if(ORG_EVENTS, logged.encode(), expected),
            )
            .await
            {
                Ok(ConditionalWrite::Written { offset }) => {
                    // Checked above against the same state, so this applies.
                    let _ = core.state.apply(&logged);
                    core.at = Some(offset);
                    core.tail = offset + 1;
                    self.publish_view(&core);
                    return Ok((Applied::Changed, Some(offset)));
                }
                Ok(ConditionalWrite::Refused { .. }) => {
                    // Another writer got in, or a new shard leader moved the
                    // tail. Only a renewal by version tells which: retrying on
                    // a lost lease would land after the new holder's record.
                    core.stale = true;
                    core.refusals += 1;
                    self.publish_lease(&core);
                    self.refresh(&mut core).await.map_err(|e| {
                        OrgError::Unavailable(format!("reading {ORG_EVENTS}: {e:#}"))
                    })?;
                    self.confirm_lease(&mut core).await?;
                }
                // A lost reply may have been a refusal: check the lease too.
                Err(e) => {
                    core.stale = true;
                    core.refusals += 1;
                    self.publish_lease(&core);
                    self.refresh_or_log(&mut core).await;
                    return Err(OrgError::Felix(e));
                }
            }
        }
        Err(OrgError::Unavailable(format!(
            "{ORG_EVENTS} kept moving under this write; try again"
        )))
    }

    /// Renews the org lease, and takes it back when it was lost. Returns when
    /// the lease has not been held for [`ORG_LEASE_GRACE`].
    pub(crate) async fn maintain(&self) {
        let mut lost_since: Option<Instant> = None;
        loop {
            // Renew first: starting the teams can take longer than the lease
            // stands, and until a renewal the tracker refuses org writes.
            let held = self.renew().await;
            if held {
                lost_since = None;
            } else if lost_since.get_or_insert_with(Instant::now).elapsed() >= ORG_LEASE_GRACE {
                return;
            }
            tokio::time::sleep(self.lease_ttl / 3).await;
        }
    }

    /// Whether the lease is held after this renewal or retake.
    async fn renew(&self) -> bool {
        let lease = self
            .core
            .lock()
            .await
            .lease
            .as_ref()
            .map(|l| (l.version, l.valid_until <= Instant::now()));
        // Felix compares a version against the live entry. Our view of the lease
        // starts before the request, so Felix's entry outlives it by up to a
        // request's latency: renew by version first, even past our expiry, and
        // take the lease again only once Felix's entry is gone.
        let renewed = match lease {
            Some((version, expired)) => Some((expired, self.renew_by(version).await)),
            None => None,
        };
        match renewed {
            Some((_, Ok(true))) => true,
            Some((false, Ok(false))) => {
                let mut core = self.core.lock().await;
                // A write's lease check may have renewed it meanwhile.
                if core.lease.as_ref().map(|l| l.version) != lease.map(|(v, _)| v) {
                    return core.lease.is_some();
                }
                eprintln!("kbl-tracker: another tracker took the org writer lease");
                core.lease = None;
                self.publish_lease(&core);
                false
            }
            // A blip: the lease stands until its own expiry passes.
            Some((_, Err(e))) => {
                eprintln!("kbl-tracker: could not renew the org writer lease: {e:#}");
                self.core
                    .lock()
                    .await
                    .lease
                    .as_ref()
                    .is_some_and(|l| l.valid_until > Instant::now())
            }
            // Lost or lapsed. Taking it back means another tracker may have
            // written meanwhile.
            None | Some((true, Ok(false))) => match self.take_lease().await {
                Ok(true) => true,
                Ok(false) => {
                    eprintln!("kbl-tracker: another tracker took the org writer lease");
                    false
                }
                Err(e) => {
                    eprintln!("kbl-tracker: could not take the org writer lease: {e:#}");
                    false
                }
            },
        }
    }

    /// Renews the lease by version, with the core lock held, so no write goes
    /// out until Felix says this tracker still holds it. A refusal leaves the
    /// lease to `maintain`: it may be a concurrent renewal that bumped the
    /// version, and writes stay refused until a renewal confirms it.
    async fn confirm_lease(&self, core: &mut Core) -> Result<(), OrgError> {
        let Some(version) = core.lease.as_ref().map(|l| l.version) else {
            return Err(OrgError::Unavailable(
                "this tracker does not hold the org writer lease".into(),
            ));
        };
        let started = Instant::now();
        let mut put = timed(self.broker.lease_put(
            self.holder.as_bytes().to_vec(),
            self.lease_ttl,
            Some(version),
        ))
        .await;
        // A renewal of ours may have landed with its reply lost: adopt its
        // version while our view stands. Past it the entry may be a retake,
        // which renew() handles with a reread.
        let live = core
            .lease
            .as_ref()
            .is_some_and(|l| l.valid_until > Instant::now());
        if !matches!(put, Ok(Some(_)))
            && live
            && let Some(ours) = self.ours().await.filter(|&v| v != version)
        {
            put = timed(self.broker.lease_put(
                self.holder.as_bytes().to_vec(),
                self.lease_ttl,
                Some(ours),
            ))
            .await;
        }
        let put = put
            .map_err(|e| OrgError::Unavailable(format!("checking the org writer lease: {e:#}")))?;
        let Some(version) = put else {
            return Err(OrgError::Unavailable(
                "another tracker may hold the org writer lease".into(),
            ));
        };
        core.lease = Some(Lease {
            version,
            valid_until: started + self.lease_ttl,
        });
        core.confirmed = core.refusals;
        Ok(())
    }

    /// Renews the lease by `version`. False when Felix no longer has it.
    async fn renew_by(&self, version: u64) -> Result<bool> {
        let (refusals, valid_until) = {
            let core = self.core.lock().await;
            let until = core.lease.as_ref().map(|l| l.valid_until);
            (core.answerable(), until)
        };
        let started = Instant::now();
        let mut put = timed(self.broker.lease_put(
            self.holder.as_bytes().to_vec(),
            self.lease_ttl,
            Some(version),
        ))
        .await;
        // A put of ours that landed with its reply lost left the entry at a
        // version we never heard of: adopt it and renew by it. With our view
        // run out it may be an Absent put that landed after another tracker
        // held the lease, so it counts as a retake.
        let mut adopted = None;
        if !matches!(put, Ok(Some(_)))
            && let Some(ours) = self.ours().await.filter(|&v| v != version)
        {
            adopted = Some(ours);
            if valid_until.is_none_or(|t| t <= Instant::now()) {
                let mut core = self.core.lock().await;
                core.stale = true;
                core.refusals += 1;
            }
            put = timed(self.broker.lease_put(
                self.holder.as_bytes().to_vec(),
                self.lease_ttl,
                Some(ours),
            ))
            .await;
        }
        let put = match put {
            Ok(put) => put,
            Err(e) => {
                // Our view still bounds the entry: whatever landed was sent
                // after it began.
                // Unless a write's lease check moved it meanwhile.
                if let Some(ours) = adopted
                    && let Some(lease) = self.core.lock().await.lease.as_mut()
                    && lease.version == version
                {
                    lease.version = ours;
                }
                return Err(e);
            }
        };
        let Some(version) = put else {
            return Ok(false);
        };
        let mut core = self.core.lock().await;
        core.lease = Some(Lease {
            version,
            valid_until: started + self.lease_ttl,
        });
        core.confirmed = core.confirmed.max(refusals);
        if core.stale {
            self.refresh_or_log(&mut core).await;
        }
        self.publish_lease(&core);
        Ok(true)
    }

    async fn take_lease(&self) -> Result<bool> {
        let refusals = self.core.lock().await.answerable();
        let started = Instant::now();
        let put = timed(self.broker.lease_put(
            self.holder.as_bytes().to_vec(),
            self.lease_ttl,
            None,
        ))
        .await;
        let held = match put {
            Ok(Some(version)) => Some((version, started + self.lease_ttl, false)),
            // The put may have landed with its reply lost. When it is ours we
            // do not know when it was granted, so our view has run out and
            // the next renewal goes by version.
            put => match self.ours().await {
                Some(version) => Some((version, Instant::now(), true)),
                None => {
                    put?;
                    None
                }
            },
        };
        Ok(match held {
            Some((version, valid_until, adopted)) => {
                // Another tracker may have written, a revoke among it, while
                // this one did not hold the lease: read before serving tokens.
                let mut core = self.core.lock().await;
                core.lease = Some(Lease {
                    version,
                    valid_until,
                });
                core.confirmed = core.confirmed.max(refusals);
                core.stale = true;
                // The grant came before the read, so a renewal by version must
                // follow it before the next write.
                core.refusals += 1;
                self.refresh_or_log(&mut core).await;
                drop(core);
                // An adopted grant has no known start, so renew by version
                // now rather than refuse writes until the next round.
                if adopted && let Err(e) = self.renew_by(version).await {
                    eprintln!("kbl-tracker: could not renew the org writer lease: {e:#}");
                }
                true
            }
            None => false,
        })
    }

    /// The version of the lease entry when it holds this tracker's name. A
    /// failed read is no answer.
    async fn ours(&self) -> Option<u64> {
        match timed(self.broker.lease_get()).await {
            Ok(Some((holder, version))) if holder == self.holder.as_bytes() => Some(version),
            _ => None,
        }
    }

    /// A failed read leaves the state stale, so agent tokens stay refused.
    async fn refresh_or_log(&self, core: &mut Core) {
        if let Err(e) = self.refresh(core).await {
            eprintln!("kbl-tracker: could not read {ORG_EVENTS}: {e:#}");
        }
    }

    async fn refresh(&self, core: &mut Core) -> Result<()> {
        let (state, at, tail) = self.read().await?;
        core.state = state;
        core.at = at;
        // Assigned, not raised: a shard failover can lower the tip.
        core.tail = tail;
        core.stale = false;
        self.publish_view(core);
        Ok(())
    }

    /// The fold of the whole log, and its committed tip. A record of a newer format stops the read:
    /// folding past it would give tables its writer did not mean.
    async fn read(&self) -> Result<(OrgState, Option<u64>, u64)> {
        let mut state = OrgState::default();
        let mut at = None;
        let (records, tail) = timed(self.broker.read_to_tip(ORG_EVENTS, 0)).await?;
        for (offset, payload) in records {
            at = Some(offset);
            match read(&payload) {
                Ok(logged) => {
                    let _ = state.apply(&logged);
                }
                Err(e @ OrgReadError::Newer { .. }) => {
                    anyhow::bail!("{ORG_EVENTS} offset {offset}: {e}")
                }
                Err(e) => eprintln!("kbl-tracker: {ORG_EVENTS} offset {offset}: {e}; skipped"),
            }
        }
        Ok((state, at, tail))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
