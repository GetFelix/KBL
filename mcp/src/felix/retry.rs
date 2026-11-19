//! Waiting out a broker that is not ready for a request yet.
//!
//! A `Connection` sends each request once. While a shard moves between brokers
//! Felix answers `retry_after` or `retry` ("shard is moving to another broker")
//! and the cluster client would back off and try again. These calls are
//! idempotent or were refused before they applied, so sending them again is safe.

use std::future::Future;
use std::time::Duration;

use felix_client::{BrokerError, ErrorCode, RetryClass};
use tokio::time::Instant;

/// How long one call keeps retrying before it reports the refusal.
const BUDGET: Duration = Duration::from_secs(30);
/// The wait when the broker names none, doubling up to `MAX_PAUSE`.
const FIRST_PAUSE: Duration = Duration::from_millis(100);
const MAX_PAUSE: Duration = Duration::from_secs(5);
/// How long a missing cache or stream is retried, as felix-client does.
const NOT_FOUND_GRACE: Duration = Duration::from_secs(5);

/// Runs `call` again while Felix says to retry it, for up to `BUDGET`.
pub(super) async fn retrying<T, F, Fut>(call: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    retrying_within(BUDGET, call).await
}

async fn retrying_within<T, F, Fut>(budget: Duration, mut call: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<T>>,
{
    let start = Instant::now();
    let end = start + budget;
    let mut pause = FIRST_PAUSE;
    loop {
        let error = match call().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let Some(broker) = error.downcast_ref::<BrokerError>() else {
            return Err(error);
        };
        if !matches!(broker.retry, RetryClass::Retry | RetryClass::RetryAfter) {
            return Err(error);
        }
        if broker.code == ErrorCode::NotFound && start.elapsed() >= NOT_FOUND_GRACE {
            return Err(error);
        }
        let asked = broker
            .detail
            .as_ref()
            .and_then(|d| d.retry_after_ms)
            .map(Duration::from_millis);
        let wait = asked.unwrap_or(pause).min(MAX_PAUSE);
        if Instant::now() + wait >= end {
            return Err(error);
        }
        tokio::time::sleep(wait).await;
        pause = (pause * 2).min(MAX_PAUSE);
    }
}

#[cfg(test)]
mod tests;
