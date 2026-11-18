//! One worker's request budget against a vendor's API limit.
//!
//! Every vendor request passes [`Budgeted`], which does three things: it
//! refuses requests while the vendor has told us to wait, it spends a token
//! bucket that polling and the reconcile slice draw from, and it reads the
//! vendor's rate limit headers. Writes (write-backs and the like) draw on a
//! reserve polling cannot touch, so a spent poll budget never stops them.
//! A worker that serves a connection takes its share of the pool's budget
//! from the tracker (the pool's hourly budget times the sources it holds
//! over the sources the pool serves), and shares the pool's backoff through
//! the tracker. See docs/design/sources.md "Request budget".

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::github::{Req, Res, Transport};

/// The longest a vendor's wait is believed.
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(3600);
/// The part of the bucket kept for writes and checks, when the pool names none.
const RESERVE: f64 = 0.2;
/// The part of that reserve wakes may spend, when the pool names none.
const WAKE_OF_RESERVE: f64 = 1.0 / 3.0;
/// Used when a vendor refuses and names no wait.
const DEFAULT_WAIT: Duration = Duration::from_secs(30);
/// GitHub asks for at least a minute after a secondary limit with no `retry-after`.
const SECONDARY_WAIT: Duration = Duration::from_secs(60);
/// The hourly limit assumed for a vendor that does not report one in a header.
const FALLBACK_HOURLY: u64 = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Vendor {
    Github,
    Jira,
    Ado,
    Gitlab,
}

impl Vendor {
    /// From the adapter's `Source::name`.
    pub(crate) fn named(name: &str) -> Self {
        match name {
            "jira" => Vendor::Jira,
            "ado" => Vendor::Ado,
            "gitlab" => Vendor::Gitlab,
            _ => Vendor::Github,
        }
    }
}

/// The vendor answered that we must wait. The adapters turn it into
/// `SourceError::RateLimited`.
#[derive(Debug)]
pub(crate) struct Limited {
    pub(crate) wait: Duration,
}

impl std::fmt::Display for Limited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rate limited; retry in {}s", self.wait.as_secs())
    }
}

impl std::error::Error for Limited {}

/// What the vendor's rate limit headers and status say about one answer.
#[derive(Debug, Default, PartialEq)]
struct Signal {
    /// Hold off this long, when the answer says so.
    wait: Option<Duration>,
    /// The request was refused for its rate, so its answer is no answer.
    refused: bool,
    /// The vendor's limit as an hourly count, when it reports one.
    hourly: Option<u64>,
    remaining: Option<u64>,
    reset_epoch: Option<u64>,
}

fn epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn signal(vendor: Vendor, res: &Res) -> Signal {
    let num = |k: &str| {
        res.headers
            .get(k)
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    let (limit, remaining, reset) = match vendor {
        Vendor::Github | Vendor::Ado => (
            num("x-ratelimit-limit"),
            num("x-ratelimit-remaining"),
            num("x-ratelimit-reset"),
        ),
        Vendor::Gitlab => (
            num("ratelimit-limit"),
            num("ratelimit-remaining"),
            num("ratelimit-reset"),
        ),
        Vendor::Jira => (None, None, None),
    };
    let retry_after = num("retry-after").map(|s| Duration::from_secs(s.max(1)));
    let message = res.body["message"].as_str().unwrap_or("").to_lowercase();
    let secondary = vendor == Vendor::Github
        && matches!(res.status, 403 | 429)
        && (message.contains("secondary rate limit") || message.contains("abuse"));
    let spent = remaining == Some(0);
    let refused = match vendor {
        Vendor::Github => {
            res.status == 429
                || (res.status == 403 && (secondary || spent || retry_after.is_some()))
        }
        _ => res.status == 429 || (res.status == 503 && retry_after.is_some()),
    };
    let until_reset = reset
        .filter(|_| spent)
        .map(|r| Duration::from_secs(r.saturating_sub(epoch()) + 1));
    let default = if secondary {
        SECONDARY_WAIT
    } else {
        DEFAULT_WAIT
    };
    let mut wait = [retry_after, until_reset].into_iter().flatten().max();
    if wait.is_none() && refused {
        wait = Some(default);
    }
    Signal {
        wait: wait.map(|w| w.min(MAX_BACKOFF)),
        refused,
        // GitLab reports its limit per minute.
        hourly: match vendor {
            Vendor::Github => limit,
            Vendor::Gitlab => limit.map(|l| l.saturating_mul(60)),
            _ => None,
        },
        remaining,
        reset_epoch: reset,
    }
}

type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// A pool's budget as the tracker's config answer gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PoolShare {
    pub(crate) per_hour: u64,
    pub(crate) kept_back: u64,
    pub(crate) wake_cap: u64,
    /// Sources every connection spending the pool serves, across teams.
    pub(crate) pool_sources: u64,
}

impl PoolShare {
    /// Reads the config answer's `budget`. None for a null or a malformed one,
    /// which leaves the worker on its own settings.
    pub(crate) fn from_config(budget: &serde_json::Value) -> Option<Self> {
        let per_hour = budget["per_hour"].as_u64().filter(|p| *p > 0)?;
        let kept_back = budget["kept_back"].as_u64()?.min(per_hour);
        Some(Self {
            per_hour,
            kept_back,
            wake_cap: budget["wake_cap"].as_u64()?.min(kept_back),
            pool_sources: budget["pool_sources"].as_u64()?.max(1),
        })
    }
}

struct Inner {
    /// None until the first use, which starts the bucket full.
    tokens: Option<f64>,
    refilled: Instant,
    /// Set by a refusal: nothing goes out, writes included.
    blocked_until: Option<Instant>,
    /// Set by an answer that says the window is spent: reads wait, writes may try.
    read_blocked_until: Option<Instant>,
    /// The vendor's own count at its last answer.
    reported_hourly: Option<u64>,
    remaining: Option<u64>,
    reset_epoch: Option<u64>,
    /// The bucket normal calls report (GitHub's `x-ratelimit-resource`).
    resource: Option<String>,
    /// What this poll's reads have spent, and what the last poll did.
    poll_spent: f64,
    last_poll_cost: f64,
    /// The pool this worker spends and its budget, once the tracker names them.
    pool: Option<String>,
    share: Option<PoolShare>,
    /// Sources this worker serves now, not counting ones handed over.
    held: u64,
    /// The pool's source count the last heartbeat answered with. It outranks
    /// a config answer, which may have been built before it.
    beat_sources: Option<u64>,
    /// Another worker's refusal, as the tracker holds it for the pool.
    pool_blocked_until: Option<Instant>,
    /// A refusal not yet told to the tracker: the longest wait.
    unsent: Option<Instant>,
    /// What wakes may still spend, refilled at the wake share's rate.
    wake_tokens: Option<f64>,
    wake_refilled: Instant,
    /// This poll was woken by a delivery, not the interval.
    waking: bool,
}

pub(crate) struct Budget {
    vendor: Vendor,
    per_hour: Option<u64>,
    share: f64,
    clock: Clock,
    inner: Mutex<Inner>,
    /// Woken when a refusal is waiting to be told to the tracker.
    pub(crate) refused: tokio::sync::Notify,
}

/// What `kbl-ingest check` reports.
#[derive(Debug)]
pub(crate) struct Snapshot {
    /// The hourly limit the vendor reported, if it does.
    pub(crate) vendor_hourly: Option<u64>,
    /// Calls the vendor says are left in its current window.
    pub(crate) vendor_remaining: Option<u64>,
    pub(crate) vendor_reset_epoch: Option<u64>,
    /// What this worker may spend an hour.
    pub(crate) hourly: u64,
    /// Tokens in the bucket now.
    pub(crate) tokens: u64,
}

impl Budget {
    pub(crate) fn new(vendor: Vendor, per_hour: Option<u64>, share: f64) -> Arc<Self> {
        Self::with_clock(vendor, per_hour, share, Arc::new(Instant::now))
    }

    pub(crate) fn with_clock(
        vendor: Vendor,
        per_hour: Option<u64>,
        share: f64,
        clock: Clock,
    ) -> Arc<Self> {
        let now = clock();
        Arc::new(Self {
            vendor,
            per_hour,
            share,
            clock,
            inner: Mutex::new(Inner {
                tokens: None,
                refilled: now,
                blocked_until: None,
                read_blocked_until: None,
                reported_hourly: None,
                remaining: None,
                reset_epoch: None,
                resource: None,
                poll_spent: 0.0,
                last_poll_cost: 0.0,
                pool: None,
                share: None,
                held: 0,
                beat_sources: None,
                pool_blocked_until: None,
                unsent: None,
                wake_tokens: None,
                wake_refilled: now,
                waking: false,
            }),
            refused: tokio::sync::Notify::new(),
        })
    }

    /// What this worker may spend an hour, reserve included: its slice of
    /// the pool's budget while the tracker gives one, else its own setting.
    fn hourly(&self, i: &Inner) -> f64 {
        let h = match i.share {
            Some(p) => {
                p.per_hour as f64 * i.held.min(p.pool_sources) as f64 / p.pool_sources as f64
            }
            None => self.per_hour.map_or_else(
                || i.reported_hourly.unwrap_or(FALLBACK_HOURLY) as f64 * self.share,
                |p| p as f64,
            ),
        };
        h.max(1.0)
    }

    /// The part of the bucket polling leaves for writes, checks and wakes.
    fn reserve(i: &Inner) -> f64 {
        i.share
            .map_or(RESERVE, |p| p.kept_back as f64 / p.per_hour as f64)
    }

    /// The part of the bucket wakes may spend, inside the reserve.
    fn wake_part(i: &Inner) -> f64 {
        i.share.map_or(RESERVE * WAKE_OF_RESERVE, |p| {
            p.wake_cap as f64 / p.per_hour as f64
        })
    }

    /// Tokens a read needs to go out. A wake poll may dip into the wake part
    /// of the reserve; writes and checks keep the rest of it.
    fn read_floor(&self, i: &Inner, hourly: f64) -> f64 {
        let wake = if i.waking { Self::wake_part(i) } else { 0.0 };
        hourly * (Self::reserve(i) - wake).max(0.0)
    }

    fn refill_wake(&self, i: &mut Inner, now: Instant) {
        let hourly = self.hourly(i);
        let part = hourly * Self::wake_part(i);
        let gained = now.saturating_duration_since(i.wake_refilled).as_secs_f64() * part / 3600.0;
        i.wake_refilled = now;
        i.wake_tokens = Some((i.wake_tokens.unwrap_or(part) + gained).min(part));
    }

    /// Sets the pool this worker spends and its budget, or clears them.
    pub(crate) fn set_pool(&self, pool: Option<String>, share: Option<PoolShare>) {
        let mut i = self.inner.lock().expect("lock");
        i.pool = pool;
        i.share = share;
        if let (Some(n), Some(share)) = (i.beat_sources, i.share.as_mut()) {
            share.pool_sources = n;
        }
    }

    /// How many sources this worker serves now.
    #[cfg(test)]
    pub(crate) fn set_held(&self, held: u64) {
        self.set_leases(held, None);
    }

    /// Sets `held` with the pool's source count the same heartbeat answered
    /// with, so a share never pairs fresh leases with a stale count.
    pub(crate) fn set_leases(&self, held: u64, pool_sources: Option<u64>) {
        let mut i = self.inner.lock().expect("lock");
        i.held = held;
        if let Some(n) = pool_sources {
            let n = n.max(1);
            i.beat_sources = Some(n);
            if let Some(share) = i.share.as_mut() {
                share.pool_sources = n;
            }
        }
    }

    pub(crate) fn pool(&self) -> Option<String> {
        self.inner.lock().expect("lock").pool.clone()
    }

    /// The longest refusal not yet told to the tracker.
    /// Kept as a deadline, so a refusal the tracker took late is told with
    /// what is left of it, not its whole wait.
    pub(crate) fn take_unsent(&self) -> Option<Duration> {
        let now = (self.clock)();
        let until = self.inner.lock().expect("lock").unsent.take()?;
        until.checked_duration_since(now).filter(|d| !d.is_zero())
    }

    pub(crate) fn now(&self) -> Instant {
        (self.clock)()
    }

    /// Records a refusal of `wait` from now.
    #[cfg(test)]
    pub(crate) fn keep_unsent(&self, wait: Duration) {
        let by = (self.clock)() + wait;
        self.restore_unsent(by);
    }

    /// Puts back a refusal that ends at `by`.
    pub(crate) fn restore_unsent(&self, by: Instant) {
        let mut i = self.inner.lock().expect("lock");
        i.unsent = Some(i.unsent.map_or(by, |u| u.max(by)));
    }

    /// The pool's backoff as the tracker holds it, `left` from now. It
    /// replaces the last one the tracker gave, so a lifted block is lifted.
    pub(crate) fn set_pool_block(&self, left: Duration) {
        let now = (self.clock)();
        let mut i = self.inner.lock().expect("lock");
        i.pool_blocked_until = (!left.is_zero()).then(|| now + left.min(MAX_BACKOFF));
    }

    fn refill(&self, i: &mut Inner, now: Instant) {
        let hourly = self.hourly(i);
        let gained = now.saturating_duration_since(i.refilled).as_secs_f64() * hourly / 3600.0;
        i.refilled = now;
        i.tokens = Some((i.tokens.unwrap_or(hourly) + gained).min(hourly));
    }

    /// Whether a request may go out now. `write` requests draw on the
    /// reserve; a read waits until the bucket is above it. The error is how
    /// long to wait.
    pub(crate) fn admit(&self, write: bool) -> Result<(), Duration> {
        let now = (self.clock)();
        let mut i = self.inner.lock().expect("lock");
        if let Some(until) = i.blocked_until {
            if until > now {
                return Err(until - now);
            }
            i.blocked_until = None;
        }
        // Another worker's refusal stops this one too, writes included.
        if let Some(until) = i.pool_blocked_until {
            if until > now {
                return Err(until - now);
            }
            i.pool_blocked_until = None;
        }
        self.refill(&mut i, now);
        if write {
            return Ok(());
        }
        if let Some(until) = i.read_blocked_until {
            if until > now {
                return Err(until - now);
            }
            i.read_blocked_until = None;
        }
        let hourly = self.hourly(&i);
        let need = self.read_floor(&i, hourly) + 1.0;
        let tokens = i.tokens.unwrap_or(hourly);
        if tokens < need {
            let secs = (need - tokens) * 3600.0 / hourly;
            return Err(Duration::from_secs_f64(secs).min(MAX_BACKOFF));
        }
        Ok(())
    }

    /// Charges one answer and reads its rate limit signals. The wait is set
    /// when the request was refused for its rate.
    fn record(&self, write: bool, metered: bool, res: &Res) -> Option<Duration> {
        let now = (self.clock)();
        let sig = signal(self.vendor, res);
        let resource = res.headers.get("x-ratelimit-resource");
        let mut i = self.inner.lock().expect("lock");
        // A token mint can be counted against another bucket (a GitHub App's
        // own), so its figures are adopted only when it names the tracked one.
        let adopt = !metered || resource.is_some_and(|r| i.resource.as_deref() == Some(r.as_str()));
        if !metered && let Some(r) = resource {
            i.resource = Some(r.clone());
        }
        if adopt && sig.hourly.is_some() {
            i.reported_hourly = sig.hourly;
        }
        if adopt && sig.remaining.is_some() {
            i.remaining = sig.remaining;
            i.reset_epoch = sig.reset_epoch;
        }
        if let Some(wait) = sig.wait {
            let until = now + wait;
            let slot = if sig.refused {
                &mut i.blocked_until
            } else {
                &mut i.read_blocked_until
            };
            *slot = Some(slot.map_or(until, |b| b.max(until)));
            if sig.refused && i.pool.is_some() {
                let by = now + wait;
                i.unsent = Some(i.unsent.map_or(by, |u| u.max(by)));
                self.refused.notify_one();
            }
        }
        // GitHub does not count a conditional request that answers 304.
        let cost = if self.vendor == Vendor::Github && res.status == 304 {
            0.0
        } else {
            1.0
        };
        self.refill(&mut i, now);
        if let Some(t) = i.tokens.as_mut() {
            *t -= cost;
        }
        if !write {
            i.poll_spent += cost;
            if i.waking {
                self.refill_wake(&mut i, now);
                if let Some(t) = i.wake_tokens.as_mut() {
                    *t -= cost;
                }
            }
        }
        sig.refused.then(|| sig.wait.unwrap_or(DEFAULT_WAIT))
    }

    /// Runs a request that does not go through [`Budgeted`], such as a token
    /// mint, under the same gate: it waits out a vendor's hold, is charged
    /// once, and its refusals are honoured. Its remaining and limit figures
    /// are adopted only when it reports the bucket normal calls do. It draws
    /// on the reserve, since nothing else can go out without the token it
    /// fetches.
    pub(crate) async fn metered<F>(&self, send: F) -> Result<Res>
    where
        F: std::future::Future<Output = Result<Res>>,
    {
        if let Err(wait) = self.admit(true) {
            return Err(Limited { wait }.into());
        }
        let res = send.await?;
        match self.record(true, true, &res) {
            Some(wait) => Err(Limited { wait }.into()),
            None => Ok(res),
        }
    }

    pub(crate) fn begin_poll(&self) {
        let mut i = self.inner.lock().expect("lock");
        i.poll_spent = 0.0;
        i.waking = false;
    }

    /// Starts a poll a delivery woke ahead of its interval. Its reads spend
    /// the wake share of the reserve, and are served after everything else.
    pub(crate) fn begin_wake_poll(&self) {
        self.begin_poll();
        self.inner.lock().expect("lock").waking = true;
    }

    /// Whether a wake may start a poll now: the wake share holds what the
    /// last poll cost, and the bucket is above what writes keep.
    pub(crate) fn wake_affordable(&self) -> bool {
        let now = (self.clock)();
        let mut i = self.inner.lock().expect("lock");
        self.refill(&mut i, now);
        self.refill_wake(&mut i, now);
        let hourly = self.hourly(&i);
        let part = hourly * Self::wake_part(&i);
        let cost = i.last_poll_cost.max(1.0);
        let floor = hourly * (Self::reserve(&i) - Self::wake_part(&i)).max(0.0);
        // Without a lease there is no share to wake on, and the clamp above
        // would drain the bucket the next lease starts from.
        (i.share.is_none() || i.held > 0)
            && part >= cost
            && i.wake_tokens.unwrap_or(part) >= cost
            && i.tokens.unwrap_or(hourly) >= floor + cost
    }

    /// A poll that stopped early spent less than a whole one would, so only
    /// a complete poll sets the cost the interval is sized from.
    pub(crate) fn end_poll(&self, complete: bool) {
        let mut i = self.inner.lock().expect("lock");
        if complete {
            i.last_poll_cost = i.poll_spent;
        }
    }

    /// How long to hold the next poll so it can be afforded: the vendor's
    /// wait, or the time to earn what the last poll cost above the reserve.
    pub(crate) fn poll_wait(&self) -> Option<Duration> {
        let now = (self.clock)();
        let mut i = self.inner.lock().expect("lock");
        let held = [i.blocked_until, i.read_blocked_until, i.pool_blocked_until]
            .into_iter()
            .flatten()
            .filter(|u| *u > now)
            .max();
        if let Some(until) = held {
            return Some(until - now);
        }
        self.refill(&mut i, now);
        let hourly = self.hourly(&i);
        let reserve = Self::reserve(&i);
        let floor = self.read_floor(&i, hourly);
        let need = floor + i.last_poll_cost.min(hourly * (1.0 - reserve));
        let deficit = need - i.tokens.unwrap_or(hourly);
        (deficit > 0.0).then(|| Duration::from_secs_f64(deficit * 3600.0 / hourly).min(MAX_BACKOFF))
    }

    /// The gap between polls: `min`, or longer when the last poll's reads
    /// would overspend the hourly budget at that rate.
    pub(crate) fn interval(&self, min: Duration) -> Duration {
        let i = self.inner.lock().expect("lock");
        let polling = self.hourly(&i) * (1.0 - Self::reserve(&i));
        stretch(min, i.last_poll_cost, polling)
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        let now = (self.clock)();
        let mut i = self.inner.lock().expect("lock");
        self.refill(&mut i, now);
        let hourly = self.hourly(&i);
        Snapshot {
            vendor_hourly: i.reported_hourly,
            vendor_remaining: i.remaining,
            vendor_reset_epoch: i.reset_epoch,
            hourly: hourly as u64,
            tokens: i.tokens.unwrap_or(hourly).max(0.0) as u64,
        }
    }
}

/// `min`, stretched so `cost` requests per poll fit `per_hour`.
fn stretch(min: Duration, cost: f64, per_hour: f64) -> Duration {
    if cost <= 0.0 || per_hour <= 0.0 {
        return min;
    }
    min.max(Duration::from_secs_f64(cost * 3600.0 / per_hour))
}

/// Requests an hour that a config spends: `listings` calls and `reads`
/// per-root reads every poll, and one read of each open issue per reconcile period.
pub(crate) fn projected_per_hour(
    listings: u64,
    reads: u64,
    open_issues: u64,
    interval: Duration,
    reconcile_period: Duration,
) -> f64 {
    let polls = 3600.0 / interval.as_secs_f64().max(1.0);
    let sweep = open_issues as f64 * 3600.0 / reconcile_period.as_secs_f64().max(1.0);
    (listings + reads) as f64 * polls + sweep
}

/// Azure DevOps reads with a POST of a query; it is a read, not a write-back.
fn is_search(url: &str) -> bool {
    url.split('?')
        .next()
        .is_some_and(|p| p.ends_with("/_apis/wit/wiql"))
}

/// A vendor transport behind the budget.
pub(crate) struct Budgeted<T> {
    inner: T,
    budget: Arc<Budget>,
}

pub(crate) fn budgeted<T>(inner: T, budget: Arc<Budget>) -> Budgeted<T> {
    Budgeted { inner, budget }
}

impl<T: Transport> Transport for Budgeted<T> {
    async fn send(&self, req: Req) -> Result<Res> {
        let write = req.method != "GET" && !is_search(&req.url);
        if let Err(wait) = self.budget.admit(write) {
            return Err(Limited { wait }.into());
        }
        let res = self.inner.send(req).await?;
        match self.budget.record(write, false, &res) {
            Some(wait) => Err(Limited { wait }.into()),
            None => Ok(res),
        }
    }
}

#[cfg(test)]
mod tests;
