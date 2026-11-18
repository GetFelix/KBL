//! One worker's request budget against a vendor's API limit.
//!
//! Every vendor request passes [`Budgeted`], which does three things: it
//! refuses requests while the vendor has told us to wait, it spends a token
//! bucket that polling and the reconcile slice draw from, and it reads the
//! vendor's rate limit headers. Writes (write-backs and the like) draw on a
//! reserve polling cannot touch, so a spent poll budget never stops them.
//! The split of one limit across several workers is the tracker's job and is
//! not done here. See docs/design/sources.md "Request budget".

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::github::{Req, Res, Transport};

/// The longest a vendor's wait is believed.
pub(crate) const MAX_BACKOFF: Duration = Duration::from_secs(3600);
/// The part of the bucket kept for writes and checks.
const RESERVE: f64 = 0.2;
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
    /// What this poll's reads have spent, and what the last poll did.
    poll_spent: f64,
    last_poll_cost: f64,
}

pub(crate) struct Budget {
    vendor: Vendor,
    per_hour: Option<u64>,
    share: f64,
    clock: Clock,
    inner: Mutex<Inner>,
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
                poll_spent: 0.0,
                last_poll_cost: 0.0,
            }),
        })
    }

    fn hourly(&self, i: &Inner) -> f64 {
        let h = self.per_hour.map_or_else(
            || i.reported_hourly.unwrap_or(FALLBACK_HOURLY) as f64 * self.share,
            |p| p as f64,
        );
        h.max(1.0)
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
        let need = hourly * RESERVE + 1.0;
        let tokens = i.tokens.unwrap_or(hourly);
        if tokens < need {
            let secs = (need - tokens) * 3600.0 / hourly;
            return Err(Duration::from_secs_f64(secs).min(MAX_BACKOFF));
        }
        Ok(())
    }

    /// Charges one answer and reads its rate limit signals. The wait is set
    /// when the request was refused for its rate.
    fn record(&self, write: bool, res: &Res) -> Option<Duration> {
        let now = (self.clock)();
        let sig = signal(self.vendor, res);
        let mut i = self.inner.lock().expect("lock");
        if sig.hourly.is_some() {
            i.reported_hourly = sig.hourly;
        }
        if sig.remaining.is_some() {
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
        }
        sig.refused.then(|| sig.wait.unwrap_or(DEFAULT_WAIT))
    }

    pub(crate) fn begin_poll(&self) {
        self.inner.lock().expect("lock").poll_spent = 0.0;
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
        let held = [i.blocked_until, i.read_blocked_until]
            .into_iter()
            .flatten()
            .filter(|u| *u > now)
            .max();
        if let Some(until) = held {
            return Some(until - now);
        }
        self.refill(&mut i, now);
        let hourly = self.hourly(&i);
        let need = hourly * RESERVE + i.last_poll_cost.min(hourly * (1.0 - RESERVE));
        let deficit = need - i.tokens.unwrap_or(hourly);
        (deficit > 0.0).then(|| Duration::from_secs_f64(deficit * 3600.0 / hourly).min(MAX_BACKOFF))
    }

    /// The gap between polls: `min`, or longer when the last poll's reads
    /// would overspend the hourly budget at that rate.
    pub(crate) fn interval(&self, min: Duration) -> Duration {
        let i = self.inner.lock().expect("lock");
        let polling = self.hourly(&i) * (1.0 - RESERVE);
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
        match self.budget.record(write, &res) {
            Some(wait) => Err(Limited { wait }.into()),
            None => Ok(res),
        }
    }
}

#[cfg(test)]
mod tests;
