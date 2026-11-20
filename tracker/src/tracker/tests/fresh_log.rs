//! A log with no records starts at the current format; one with records does not.

use super::*;
use kbl_core::work::FORMAT;

fn fresh() -> Settings {
    Settings {
        fresh_format: FORMAT,
        ..settings()
    }
}

#[tokio::test]
async fn a_new_log_starts_at_the_current_format() {
    let fake = Fake::default();
    let t = start_with(&fake, fresh()).await;
    assert_eq!(t.0.core.lock().await.state.format(), FORMAT);
    let log = fake.log();
    assert_eq!(log.len(), 1, "the raise is the log's first record");
    assert!(matches!(
        log[0].record,
        WorkRecord::FormatRaised { to } if to == FORMAT
    ));
    assert_eq!(log[0].v, FORMAT);
    // A worker starts with no raise by hand.
    t.start_worker(
        &agent("sup"),
        &kbl_core::org::TokenHash::of("tok-sup"),
        crate::tracker::NewWorker {
            worker: agent("sup-w1"),
            vendor: None,
            profile: None,
            model: None,
        },
    )
    .await
    .unwrap();
    // A restart finds records and writes no second raise.
    let again = start_with(&fake, fresh()).await;
    assert_eq!(again.0.core.lock().await.state.format(), FORMAT);
    assert_eq!(fake.log().len(), 2);
}

#[tokio::test]
async fn a_log_with_records_keeps_its_format() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    let before = fake.log().len();
    let t = start_with(&fake, fresh()).await;
    assert_eq!(t.0.core.lock().await.state.format(), 1);
    assert_eq!(fake.log().len(), before);
}

#[tokio::test]
async fn a_format_refusal_says_what_is_needed_and_how() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let err = t
        .start_worker(
            &agent("sup"),
            &kbl_core::org::TokenHash::of("tok-sup"),
            crate::tracker::NewWorker {
                worker: agent("sup-w1"),
                vendor: None,
                profile: None,
                model: None,
            },
        )
        .await
        .unwrap_err();
    let Error::Format(m) = &err else {
        panic!("{err}");
    };
    assert!(
        m.contains("worker_started needs log format 21, this log is at 1")
            && m.contains("a person raises it with format_raised")
            && m.contains("docs/upgrades.md#formats"),
        "{m}"
    );
}
