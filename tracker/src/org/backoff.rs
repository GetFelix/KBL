//! A budget pool's shared backoff: when any worker is told to wait by the
//! vendor, every worker spending that pool waits. One cache entry per pool,
//! in the org's cache, because a pool spans teams.

use kbl_core::names::Key;
use serde::{Deserialize, Serialize};

use super::{Org, OrgError, now_ms};
use crate::felix::Broker;

/// The longest a pool is held off, whatever a worker reports.
pub(crate) const MAX_BACKOFF_MS: u64 = 3_600_000;

/// Tries at the entry before a write gives up.
const TRIES: usize = 6;

#[derive(Serialize, Deserialize)]
struct Entry {
    /// Tracker wall clock, in milliseconds since the epoch.
    until_ms: u64,
}

fn down(e: impl std::fmt::Display) -> OrgError {
    OrgError::Unavailable(format!("the pool backoff: {e}"))
}

impl<B: Broker> Org<B> {
    /// Milliseconds left on `pool`'s backoff, zero when it has none. The
    /// answer is relative, so a worker's clock never has to match ours.
    pub(crate) async fn pool_backoff(&self, pool: &Key) -> Result<u64, OrgError> {
        let read = self.broker.pool_backoff_get(pool.as_str()).await;
        let entry = read.map_err(down)?;
        let until = match entry {
            Some((bytes, _)) => {
                serde_json::from_slice::<Entry>(&bytes)
                    .map_err(down)?
                    .until_ms
            }
            None => 0,
        };
        Ok(until.saturating_sub(now_ms()))
    }

    /// Holds `pool` off for `wait_ms` from now, capped at an hour. The later
    /// of this and what is already there wins. Answers what is left.
    pub(crate) async fn raise_pool_backoff(
        &self,
        pool: &Key,
        wait_ms: u64,
    ) -> Result<u64, OrgError> {
        let target = now_ms().saturating_add(wait_ms.min(MAX_BACKOFF_MS));
        for _ in 0..TRIES {
            let read = self.broker.pool_backoff_get(pool.as_str()).await;
            let entry = read.map_err(down)?;
            let (held, version) = match entry {
                Some((bytes, v)) => (
                    serde_json::from_slice::<Entry>(&bytes)
                        .map_err(down)?
                        .until_ms,
                    Some(v),
                ),
                None => (0, None),
            };
            if held >= target {
                return Ok(held.saturating_sub(now_ms()));
            }
            let bytes = serde_json::to_vec(&Entry { until_ms: target }).map_err(down)?;
            if self
                .broker
                .pool_backoff_put(pool.as_str(), bytes, version)
                .await
                .map_err(down)?
                .is_some()
            {
                return Ok(target.saturating_sub(now_ms()));
            }
        }
        Err(down("kept losing the write to another tracker"))
    }
}
