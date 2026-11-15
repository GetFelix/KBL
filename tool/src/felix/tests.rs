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
