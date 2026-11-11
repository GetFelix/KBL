//! The loop that keeps a held claim alive, shared by the task tools (which
//! extend the delivery on Felix) and the work tools (which ask the tracker).

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::time::Instant;

use crate::felix::ExtendError;

/// Extensions in a row that may fail before the model is warned its claim is at risk.
pub(crate) const RISK_AFTER: u32 = 2;

/// What the extend loop has seen, for the next tool call to report.
pub(crate) struct Health {
    /// The claim lapsed or was handed out again.
    pub(crate) lost: bool,
    /// Extensions that failed since the last one that worked.
    pub(crate) failures: u32,
    pub(crate) last_ok: Instant,
}

impl Health {
    pub(crate) fn fresh() -> Arc<StdMutex<Health>> {
        Arc::new(StdMutex::new(Health {
            lost: false,
            failures: 0,
            last_ok: Instant::now(),
        }))
    }
}

/// One held claim, as the loop sees it.
pub(crate) trait Claim: Send + Sync + 'static {
    fn extend(&self) -> impl Future<Output = Result<Duration, ExtendError>> + Send;
    /// The claim stands for `visible` more.
    fn extended(&self, visible: Duration) -> impl Future<Output = ()> + Send;
    /// An extension failed.
    fn failed(&self, error: &anyhow::Error) -> impl Future<Output = ()> + Send;
    /// The claim is gone.
    fn lost(&self) -> impl Future<Output = ()> + Send;
}

/// Extends the claim until this loop is aborted or the claim is lost.
pub(crate) async fn keep_alive<C: Claim>(
    claim: C,
    every: Duration,
    visibility: Duration,
    health: Arc<StdMutex<Health>>,
) {
    loop {
        tokio::time::sleep(every).await;
        // A hung extend counts as a failed one, or the lost threshold never comes.
        let extended = tokio::time::timeout(every, claim.extend())
            .await
            .unwrap_or_else(|_| Err(ExtendError::Failed(anyhow::anyhow!("the extend timed out"))));
        let gone = match extended {
            Ok(visible) => {
                {
                    let mut h = health.lock().expect("health lock");
                    h.failures = 0;
                    h.last_ok = Instant::now();
                }
                claim.extended(visible).await;
                false
            }
            Err(e) => {
                claim.failed(e.error()).await;
                let mut h = health.lock().expect("health lock");
                h.failures += 1;
                // Lapsed or handed out again, or too long without a working
                // extension for the claim to still stand.
                matches!(e, ExtendError::Stale(_)) || h.last_ok.elapsed() >= visibility
            }
        };
        if gone {
            health.lock().expect("health lock").lost = true;
            claim.lost().await;
            return;
        }
    }
}

pub(crate) fn at_risk(health: &StdMutex<Health>) -> Option<&'static str> {
    (health.lock().expect("health lock").failures >= RISK_AFTER).then_some(
        "claim at risk: the last extensions were not accepted, so the work may be handed to another agent soon",
    )
}
