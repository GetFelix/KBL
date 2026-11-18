//! The worker's side of a pool's shared backoff. A refusal from the vendor is
//! told to the tracker, which keeps the later time per budget pool, and the
//! pool's time is read back so every worker of the pool stops, not only the
//! one that was refused. Design: docs/design/sources.md "Request budget".

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::budget::Budget;
use crate::tracker_api::Http;

/// How often the pool's backoff is read when no refusal wakes the task.
const READ_EVERY: Duration = Duration::from_secs(10);

/// The two calls on `/v1/ingest/backoff/{pool}`.
pub(crate) trait PoolApi: Send + Sync {
    /// Holds the pool off for `wait` more, or only reads it when none is
    /// given. Answers what is left of the pool's backoff.
    fn backoff(
        &self,
        pool: &str,
        wait: Option<Duration>,
    ) -> impl Future<Output = Result<Duration>> + Send;
}

impl PoolApi for Http {
    async fn backoff(&self, pool: &str, wait: Option<Duration>) -> Result<Duration> {
        let (status, body) = match wait {
            Some(w) => self.pool_backoff_put(pool, w.as_secs().max(1)).await?,
            None => self.pool_backoff_get(pool).await?,
        };
        match (status, body["remaining_ms"].as_u64()) {
            (200, Some(ms)) => Ok(Duration::from_millis(ms)),
            // A tracker that predates the route keeps no shared backoff.
            (404 | 405, _) => Ok(Duration::ZERO),
            _ => bail!("the tracker answered {status} to the pool backoff call: {body}"),
        }
    }
}

/// Tells the tracker the longest refusal not yet told, then reads the pool's
/// backoff into the budget. A refusal the tracker could not take is kept for
/// the next round.
pub(crate) async fn sync_once<A: PoolApi>(api: &A, budget: &Budget) {
    let Some(pool) = budget.pool() else { return };
    if let Some(wait) = budget.take_unsent() {
        match api.backoff(&pool, Some(wait)).await {
            Ok(left) => budget.set_pool_block(left),
            Err(e) => {
                budget.keep_unsent(wait);
                eprintln!("kbl-ingest: could not share the rate limit: {e:#}");
            }
        }
        return;
    }
    match api.backoff(&pool, None).await {
        Ok(left) => budget.set_pool_block(left),
        Err(e) => eprintln!("kbl-ingest: could not read the pool's backoff: {e:#}"),
    }
}

/// Runs [`sync_once`] on every refusal and every few seconds.
pub(crate) async fn task<A: PoolApi>(api: Arc<A>, budget: Arc<Budget>) {
    loop {
        sync_once(&*api, &budget).await;
        tokio::select! {
            () = budget.refused.notified() => {}
            () = tokio::time::sleep(READ_EVERY) => {}
        }
    }
}

#[cfg(test)]
mod tests;
