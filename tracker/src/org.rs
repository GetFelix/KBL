//! The org writer: the fold of `org.events` and the one place that appends to
//! it. It holds a lease of its own, the `writer` key in `org.tracker`, so a
//! second tracker cannot write the org log while this one does. See "One
//! tracker, many teams" in docs/design.md.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use kbl_core::codec::Codec;
use kbl_core::names::Key;
use kbl_core::org::{ORG_EVENTS, OrgLogged, OrgReadError, OrgRecord, read};
use kbl_core::org_fold::{Applied, OrgState};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::felix::Broker;

/// How long the org lease may stay lost before the process exits. Past it,
/// revocations would stop taking effect.
pub(crate) const ORG_LEASE_GRACE: Duration = Duration::from_secs(60);

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
}

pub(crate) struct Org<B> {
    broker: B,
    core: Mutex<Core>,
    lease_ttl: Duration,
    holder: String,
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
            }),
            lease_ttl,
            holder: format!("tracker-{}-{}", std::process::id(), now_ms()),
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
        org.refresh(&mut core).await?;
        drop(core);
        Ok(org)
    }

    /// The org as last folded.
    pub(crate) async fn state(&self) -> OrgState {
        self.core.lock().await.state.clone()
    }

    /// Appends `record` if the fold takes it and it changes something. The
    /// answer says whether it changed anything, and where it landed.
    pub(crate) async fn append(
        &self,
        by: Key,
        record: OrgRecord,
    ) -> Result<(Applied, Option<u64>), OrgError> {
        let mut core = self.core.lock().await;
        let margin = self.lease_ttl / 3;
        if !core
            .lease
            .as_ref()
            .is_some_and(|l| l.valid_until > Instant::now() + margin)
        {
            return Err(OrgError::Unavailable(
                "this tracker does not hold the org writer lease".into(),
            ));
        }
        if core.stale {
            self.refresh(&mut core)
                .await
                .map_err(|e| OrgError::Unavailable(format!("reading {ORG_EVENTS}: {e:#}")))?;
        }
        let logged = OrgLogged::new(now_ms(), by, record);
        match core.state.check(&logged) {
            Err(why) => return Err(OrgError::Refused(why.why())),
            Ok(Applied::Unchanged) => return Ok((Applied::Unchanged, None)),
            Ok(Applied::Changed) => {}
        }
        let guess = core.at.map_or(0, |a| a + 1);
        match self.broker.publish(ORG_EVENTS, logged.encode()).await {
            Ok(offset) => {
                // Checked above against the same state, so this applies.
                let _ = core.state.apply(&logged);
                core.at = Some(offset);
                if offset != guess {
                    core.stale = true;
                }
                Ok((Applied::Changed, Some(offset)))
            }
            Err(e) => {
                core.stale = true;
                Err(OrgError::Felix(e))
            }
        }
    }

    /// Renews the org lease, and takes it back when it was lost. Returns when
    /// the lease has not been held for [`ORG_LEASE_GRACE`].
    pub(crate) async fn maintain(&self) {
        let mut lost_since: Option<Instant> = None;
        loop {
            tokio::time::sleep(self.lease_ttl / 3).await;
            let held = self.renew().await;
            if held {
                lost_since = None;
            } else if lost_since.get_or_insert_with(Instant::now).elapsed() >= ORG_LEASE_GRACE {
                return;
            }
        }
    }

    /// Whether the lease is held after this renewal or retake.
    async fn renew(&self) -> bool {
        let expect = self.core.lock().await.lease.as_ref().map(|l| l.version);
        let Some(version) = expect else {
            // Taking it back means another tracker may have written meanwhile.
            return match self.take_lease().await {
                Ok(true) => {
                    self.core.lock().await.stale = true;
                    true
                }
                Ok(false) => false,
                Err(e) => {
                    eprintln!("kbl-tracker: could not take the org writer lease: {e:#}");
                    false
                }
            };
        };
        let started = Instant::now();
        let put = self
            .broker
            .lease_put(
                self.holder.as_bytes().to_vec(),
                self.lease_ttl,
                Some(version),
            )
            .await;
        let mut core = self.core.lock().await;
        match put {
            Ok(Some(version)) => {
                core.lease = Some(Lease {
                    version,
                    valid_until: started + self.lease_ttl,
                });
                true
            }
            Ok(None) => {
                eprintln!("kbl-tracker: another tracker took the org writer lease");
                core.lease = None;
                false
            }
            // A blip: the lease stands until its own expiry passes.
            Err(e) => {
                eprintln!("kbl-tracker: could not renew the org writer lease: {e:#}");
                core.lease
                    .as_ref()
                    .is_some_and(|l| l.valid_until > Instant::now())
            }
        }
    }

    async fn take_lease(&self) -> Result<bool> {
        let started = Instant::now();
        let put = self
            .broker
            .lease_put(self.holder.as_bytes().to_vec(), self.lease_ttl, None)
            .await?;
        Ok(match put {
            Some(version) => {
                self.core.lock().await.lease = Some(Lease {
                    version,
                    valid_until: started + self.lease_ttl,
                });
                true
            }
            None => false,
        })
    }

    async fn refresh(&self, core: &mut Core) -> Result<()> {
        let (state, at) = self.read().await?;
        core.state = state;
        core.at = at;
        core.stale = false;
        Ok(())
    }

    /// The fold of the whole log. A record of a newer format stops the read:
    /// folding past it would give tables its writer did not mean.
    async fn read(&self) -> Result<(OrgState, Option<u64>)> {
        let mut state = OrgState::default();
        let mut at = None;
        for (offset, payload) in self.broker.read_from(ORG_EVENTS, 0).await? {
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
        Ok((state, at))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
