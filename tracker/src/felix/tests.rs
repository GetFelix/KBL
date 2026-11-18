use anyhow::Context;
use felix_client::{CursorErrorReason, SubscribeCursorError};

use serde_json::json;

use super::{acks_are_durable, pages_to_tip, past_the_tail, settle_with};

fn refused(reason: CursorErrorReason) -> anyhow::Error {
    anyhow::Error::from(SubscribeCursorError {
        reason,
        requested: 9,
        available: 4,
        trim_headers: None,
    })
}

/// Only an offset past the tail reads as an empty tip. Retention's refusal
/// and any other failure stay errors, so the connection is dropped for them.
#[test]
fn only_a_read_past_the_tail_is_the_tip() {
    assert!(past_the_tail(&refused(CursorErrorReason::InFuture)));
    let wrapped = Err::<(), _>(refused(CursorErrorReason::InFuture)).context("read");
    assert!(past_the_tail(&wrapped.unwrap_err()));
    assert!(!past_the_tail(&refused(CursorErrorReason::TooOld)));
    assert!(!past_the_tail(&anyhow::anyhow!("the connection dropped")));
}

/// Like Felix's read: a page ends after its last record, and the empty page
/// after it names the stop, which a generation start can push past the last
/// record. The tip is that stop, so the next append lands where Felix wants it.
#[tokio::test]
async fn the_tip_is_the_empty_pages_offset_past_skipped_offsets() {
    // Records at 0..=2; a new shard leader's generation start took offset 3.
    let page = |from: u64| async move {
        Ok(match from {
            0 => ((0..3).map(|o| (o, vec![1])).collect(), 3),
            3 => (Vec::new(), 4),
            _ => return Err(refused(CursorErrorReason::InFuture)),
        })
    };
    let (records, tip) = pages_to_tip("s", 0, page).await.unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(tip, 4);
    // Read from past the stop, Felix refuses the offset: that is the tip too.
    assert_eq!(pages_to_tip("s", 5, page).await.unwrap(), (Vec::new(), 5));
}

/// A Raft shard's refused conditional publish leaves a blank, so the log can
/// end in several: the empty page after the last record names the stop past
/// all of them, and a write at the tip lands after the blanks.
#[tokio::test]
async fn the_tip_passes_trailing_blanks() {
    // Records at 0..=2; blanks at 3, 4 and 5.
    let page = |from: u64| async move {
        Ok(match from {
            0 => ((0..3).map(|o| (o, vec![1])).collect(), 3),
            3..=5 => (Vec::new(), 6),
            _ => return Err(refused(CursorErrorReason::InFuture)),
        })
    };
    let (records, tip) = pages_to_tip("s", 0, page).await.unwrap();
    assert_eq!((records.len(), tip), (3, 6));
    // Rebuilt from a cursor that sits on a blank.
    assert_eq!(pages_to_tip("s", 4, page).await.unwrap(), (Vec::new(), 6));
}

fn work_events(durable: bool, consistency: &str, copies: u64) -> serde_json::Value {
    json!({
        "stream": "work.events",
        "durable": durable,
        "consistency": consistency,
        "replication_factor": copies,
    })
}

/// The tracker starts only on a work.events whose acks a failover cannot
/// undo: a durable quorum, or a durable single copy that never fails over.
#[test]
fn work_events_must_be_acknowledged_durably() {
    assert!(acks_are_durable(&work_events(true, "Quorum", 3)).is_ok());
    assert!(acks_are_durable(&work_events(true, "Leader", 1)).is_ok());
    assert!(acks_are_durable(&work_events(true, "Leader", 3)).is_err());
    assert!(acks_are_durable(&work_events(false, "Leader", 1)).is_err());
    assert!(acks_are_durable(&work_events(false, "Quorum", 3)).is_err());
    let err = acks_are_durable(&work_events(true, "Leader", 2)).unwrap_err();
    assert!(err.to_string().contains("replication_factor: 2"), "{err}");
}

/// A stream whose copy count the control plane left out is not one copy.
#[test]
fn a_missing_replication_factor_is_refused() {
    let stream = json!({ "durable": true, "consistency": "Leader" });
    let err = acks_are_durable(&stream).unwrap_err();
    assert!(
        err.to_string().contains("replication_factor: unknown"),
        "{err}"
    );
    let mut quorum = stream.clone();
    quorum["consistency"] = json!("Quorum");
    assert!(acks_are_durable(&quorum).is_ok());
}

/// A broker that has not settled refuses the first calls; startup waits it out
/// instead of exiting, and still reports an error that outlasts the wait.
#[tokio::test]
async fn startup_retries_until_the_broker_settles() {
    use std::cell::Cell;
    use std::time::Duration;
    let tries = Cell::new(0);
    let ms = Duration::from_millis(1);
    let ok = settle_with(
        "test",
        || {
            tries.set(tries.get() + 1);
            let n = tries.get();
            async move {
                if n < 4 {
                    anyhow::bail!("the lease lapsed")
                }
                Ok(n)
            }
        },
        Duration::from_secs(30),
        ms,
    )
    .await;
    assert_eq!(ok.unwrap(), 4);
    let never = settle_with(
        "test",
        || async { Err::<(), _>(anyhow::anyhow!("still down")) },
        Duration::from_millis(20),
        ms,
    )
    .await;
    assert!(never.unwrap_err().to_string().contains("still down"));
}
