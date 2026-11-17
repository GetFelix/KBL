//! Ingest source leases: which worker of a group polls a source, and the
//! rules a lease moves by. The tracker keeps each lease as one cache entry
//! written with a version check; these are the decisions it makes about it.
//! Design: docs/design/sources.md#state-leases-and-workers.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How long a lease stands after its last renew, by the tracker's clock.
pub const LEASE_MS: u64 = 90_000;

/// How often a worker renews all its leases.
pub const HEARTBEAT_MS: u64 = 30_000;

/// How old a worker's last successful renew may be, by its own monotonic
/// clock from when it sent the renew, before it makes no more upstream calls.
/// 30 s inside [`LEASE_MS`], so the old holder has stopped before the lease
/// can expire and move.
pub const FENCE_MS: u64 = 60_000;

/// One source's lease as the cache holds it. A released or expired lease keeps
/// its epoch, so the next take is still numbered after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// The worker that holds it, none once released.
    pub holder: Option<String>,
    /// Raised by one on every take. Milestone 2 fences state and record
    /// writes with it.
    pub epoch: u64,
    /// Milliseconds since the epoch, by the tracker's clock.
    pub expires_ms: u64,
}

/// What the tracker does with one source for the worker asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The worker holds it: push the expiry out, same epoch.
    Renew,
    /// Nobody holds it: the worker takes it with the next epoch.
    Take,
    /// Another worker holds it, or it is not this worker's share.
    Leave,
}

impl Lease {
    pub fn held_by(&self, who: &str, now_ms: u64) -> bool {
        self.holder.as_deref() == Some(who) && self.expires_ms > now_ms
    }

    /// Free to take: never held, released, or expired.
    pub fn free(&self, now_ms: u64) -> bool {
        self.holder.is_none() || self.expires_ms <= now_ms
    }
}

/// The step for worker `me` on a source whose lease is `current`, when the
/// rendezvous hash `assigned` it to `me` or not. A lease moves only once it is
/// free, so an assignment that changed does not take it from a live holder;
/// a holder that is no longer assigned is not renewed and lets it run out.
pub fn step(current: Option<&Lease>, me: &str, assigned: bool, now_ms: u64) -> Step {
    if !assigned {
        return Step::Leave;
    }
    match current {
        None => Step::Take,
        // Renewed even past its expiry while nobody took it: the epoch did
        // not move, so no one else polled in between.
        Some(l) if l.holder.as_deref() == Some(me) => Step::Renew,
        Some(l) if l.free(now_ms) => Step::Take,
        Some(_) => Step::Leave,
    }
}

/// The lease `step` writes, or none for [`Step::Leave`].
pub fn next(current: Option<&Lease>, me: &str, step: Step, now_ms: u64) -> Option<Lease> {
    let epoch = current.map_or(0, |l| l.epoch);
    let epoch = match step {
        Step::Leave => return None,
        Step::Renew => epoch,
        Step::Take => epoch + 1,
    };
    Some(Lease {
        holder: Some(me.to_owned()),
        epoch,
        expires_ms: now_ms + LEASE_MS,
    })
}

/// The member of `members` that rendezvous hashing gives `source`: the one with
/// the highest hash of the pair. A member joining or leaving moves only the
/// sources it wins or held.
pub fn owner<'a>(source: &str, members: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    members.into_iter().max_by_key(|&m| score(source, m))
}

fn score<'a>(source: &str, member: &'a str) -> ([u8; 32], &'a str) {
    let mut h = Sha256::new();
    h.update(source.as_bytes());
    h.update([0]);
    h.update(member.as_bytes());
    // The member breaks a tie, so every tracker picks the same one.
    (h.finalize().into(), member)
}

#[cfg(test)]
mod tests;
