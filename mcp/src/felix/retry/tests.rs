use std::sync::atomic::{AtomicUsize, Ordering};

use felix_client::{BrokerError, ErrorCode, ErrorDetail};

use super::*;

fn moving(after_ms: u64) -> anyhow::Error {
    let mut e = BrokerError::new(
        ErrorCode::ShardUnavailable,
        RetryClass::RetryAfter,
        "cache put not served: shard is moving to another broker",
    );
    e.detail = Some(ErrorDetail {
        reason: Some("moving".into()),
        retry_after_ms: Some(after_ms),
    });
    e.into()
}

#[tokio::test(start_paused = true)]
async fn a_put_during_a_move_lands_once_the_move_ends() {
    let calls = AtomicUsize::new(0);
    let started = Instant::now();
    let out = retrying(|| async {
        if calls.fetch_add(1, Ordering::SeqCst) < 3 {
            Err(moving(500))
        } else {
            Ok(())
        }
    })
    .await;
    assert!(out.is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert!(
        started.elapsed() >= Duration::from_millis(1500),
        "honors retry_after_ms"
    );
}

#[tokio::test(start_paused = true)]
async fn a_move_that_never_ends_reports_the_refusal() {
    let calls = AtomicUsize::new(0);
    let err = retrying_within(Duration::from_secs(5), || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Err::<(), _>(moving(1000))
    })
    .await
    .unwrap_err();
    assert!(format!("{err:#}").contains("shard is moving"));
    assert!(calls.load(Ordering::SeqCst) <= 5);
}

#[tokio::test(start_paused = true)]
async fn a_refusal_that_will_not_change_is_not_retried() {
    let calls = AtomicUsize::new(0);
    let err = retrying(|| async {
        calls.fetch_add(1, Ordering::SeqCst);
        Err::<(), _>(anyhow::Error::from(BrokerError::new(
            ErrorCode::StaleClaim,
            RetryClass::Fatal,
            "no",
        )))
    })
    .await;
    assert!(err.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn a_missing_cache_is_given_up_on_after_the_grace() {
    let started = Instant::now();
    let err = retrying(|| async {
        Err::<(), _>(anyhow::Error::from(BrokerError::new(
            ErrorCode::NotFound,
            RetryClass::RetryAfter,
            "no such cache",
        )))
    })
    .await;
    assert!(err.is_err());
    let took = started.elapsed();
    assert!(
        took >= Duration::from_secs(5) && took < Duration::from_secs(12),
        "{took:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_pause_over_two_seconds_is_honored() {
    let calls = AtomicUsize::new(0);
    let started = Instant::now();
    retrying(|| async {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(moving(4000))
        } else {
            Ok(())
        }
    })
    .await
    .unwrap();
    assert!(started.elapsed() >= Duration::from_secs(4));
}
