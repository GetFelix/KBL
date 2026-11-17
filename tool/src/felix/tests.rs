use serde_json::json;

use super::*;

fn reply(streams: &[&str], caches: &[&str]) -> Value {
    let items = |statuses: &[&str]| -> Vec<Value> {
        statuses.iter().map(|s| json!({ "status": s })).collect()
    };
    json!({ "streams": items(streams), "caches": items(caches) })
}

#[test]
fn a_new_run_has_every_item_created() {
    let body = reply(&["created"; 3], &["created"; 6]);
    assert_eq!(outcome(&body).unwrap(), Outcome::AllNew);
}

#[test]
fn a_repeat_has_every_item_unchanged() {
    let body = reply(&["unchanged"; 3], &["unchanged"; 6]);
    assert_eq!(outcome(&body).unwrap(), Outcome::NoneNew);
}

#[test]
fn a_run_with_some_resources_already_there_is_mixed() {
    let body = reply(&["unchanged", "created", "created"], &["created"; 6]);
    assert_eq!(outcome(&body).unwrap(), Outcome::Mixed);
}

#[test]
fn a_reply_that_is_not_a_batch_result_is_an_error() {
    assert!(outcome(&json!({})).is_err());
    assert!(outcome(&json!({ "streams": [], "caches": [{}] })).is_err());
}

#[test]
fn only_a_shard_that_is_not_ready_is_retried() {
    let not_ready =
        anyhow::anyhow!("group seek not served: this broker is still opening the shard")
            .context("create group workers on agents.tasks.demo");
    assert!(is_not_ready(&not_ready));
    assert!(!is_not_ready(&anyhow::anyhow!("permission denied")));
}

#[test]
fn a_lapsed_lease_is_leaderless() {
    let err = anyhow::anyhow!("publish_if refused: publish not served: lease lapsed: shard 3");
    assert!(is_leaderless(&err));
    assert!(is_leaderless(&anyhow::anyhow!("lease lapsed")));
}

#[test]
fn an_unserved_publish_is_leaderless() {
    assert!(is_leaderless(&anyhow::anyhow!("publish not served: moved")));
    assert!(!is_leaderless(&anyhow::anyhow!("permission denied")));
}

#[tokio::test]
async fn a_shard_that_is_not_ready_is_tried_again_until_it_is() {
    let mut calls = 0;
    let got = placed(|| {
        calls += 1;
        let ready = calls == 3;
        async move {
            if ready {
                Ok(7)
            } else {
                Err(anyhow::anyhow!("shard is not assigned to any broker"))
            }
        }
    })
    .await
    .unwrap();
    assert_eq!((got, calls), (7, 3));
}

#[tokio::test]
async fn any_other_failure_is_returned_at_once() {
    let mut calls = 0;
    let got: Result<()> = placed(|| {
        calls += 1;
        async { Err(anyhow::anyhow!("permission denied")) }
    })
    .await;
    assert!(got.is_err());
    assert_eq!(calls, 1);
}

#[tokio::test]
async fn a_read_waits_out_a_shard_that_is_not_ready() {
    let mut calls = 0;
    let (records, tip) = pages_to_tip("org.events", |from| {
        calls += 1;
        let n = calls;
        async move {
            match (n, from) {
                (1..=2, _) => Err(anyhow::anyhow!(
                    "read refused: shard is not assigned to any broker"
                )),
                (_, 0) => Ok((vec![(0, b"a".to_vec())], 1)),
                _ => Ok((Vec::new(), from)),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!((records.len(), tip, calls), (1, 1, 4));
}

#[tokio::test]
async fn a_read_refused_for_another_reason_is_not_retried() {
    let mut calls = 0;
    let got = pages_to_tip("org.events", |_| {
        calls += 1;
        async { Err(anyhow::anyhow!("permission denied")) }
    })
    .await;
    assert!(got.is_err());
    assert_eq!(calls, 1);
}

#[test]
fn a_refused_read_on_an_unplaced_shard_is_leaderless() {
    let err = anyhow::anyhow!("read refused: shard is not assigned to any broker")
        .context("read work.events from offset 0");
    assert!(is_leaderless(&err));
}

#[tokio::test(start_paused = true)]
async fn a_read_refused_for_a_leaderless_shard_succeeds_once_it_has_a_leader() {
    let mut calls = 0;
    let (records, tip) = pages_to_tip("work.events", |from| {
        calls += 1;
        let n = calls;
        async move {
            match (n, from) {
                (1..=5, _) => Err(anyhow::anyhow!(
                    "read refused: shard is not assigned to any broker"
                )),
                (_, 0) => Ok((vec![(0, b"a".to_vec())], 1)),
                _ => Ok((Vec::new(), from)),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!((records.len(), tip, calls), (1, 1, 7));
}

#[tokio::test(start_paused = true)]
async fn a_read_that_never_recovers_fails_within_the_bound() {
    let start = Instant::now();
    let err = pages_to_tip("work.events", |_| async {
        Err(anyhow::anyhow!(
            "read refused: shard is not assigned to any broker"
        ))
    })
    .await
    .unwrap_err();
    let waited = start.elapsed();
    assert!(
        format!("{err:#}").contains("read work.events from offset 0"),
        "{err:#}"
    );
    assert!(
        waited >= LEADERLESS_WAIT && waited < LEADERLESS_WAIT + Duration::from_secs(5),
        "{waited:?}"
    );
}

#[tokio::test]
async fn a_read_with_no_records_but_a_moving_cursor_keeps_reading() {
    // Refused conditional publishes leave blank offsets: a page can hold none
    // and still not be the end.
    let (records, tip) = pages_to_tip("work.events", |from| async move {
        Ok(match from {
            0 => (vec![(0, b"a".to_vec())], 1),
            1 => (Vec::new(), 3),
            3 => (vec![(3, b"b".to_vec())], 4),
            _ => (Vec::new(), from),
        })
    })
    .await
    .unwrap();
    assert_eq!((records.len(), tip), (2, 4));
}

#[test]
fn a_missing_or_forbidden_stream_is_not_leaderless() {
    for text in [
        "stream work.events not found",
        "namespace team is not assigned to this tenant",
        "permission denied: not owner of stream work.events",
        "read refused: not the owner",
    ] {
        assert!(!is_leaderless(&anyhow::anyhow!(text)), "{text}");
    }
}

#[tokio::test]
async fn a_read_of_a_missing_stream_fails_at_once() {
    let mut calls = 0;
    let got = pages_to_tip("work.events", |_| {
        calls += 1;
        async { Err(anyhow::anyhow!("stream work.events not found")) }
    })
    .await;
    assert!(got.is_err());
    assert_eq!(calls, 1);
}
