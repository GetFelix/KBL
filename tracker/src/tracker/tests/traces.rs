//! The fold's spans: one per applied record, and one per rebuild.

use super::*;

#[tokio::test]
async fn a_record_and_a_rebuild_each_get_a_span() {
    let capture = kbl_trace::capture();
    let fake = Fake::default();
    let t = start_with(
        &fake,
        Settings {
            team: "t-spans".into(),
            ..settings()
        },
    )
    .await;
    seed(&t, "a1", 2).await;
    t.lease().stale = true;
    t.begin().await.unwrap();

    let spans = capture.finished();
    let applied: Vec<_> = spans
        .iter()
        .filter(|s| s.name == "tracker.fold.apply")
        .collect();
    assert!(!applied.is_empty(), "{spans:#?}");
    assert!(
        applied
            .iter()
            .any(|s| s.attributes["record.kind"] == "imported")
    );
    assert!(applied.iter().all(|s| s.attributes["team"] == "t-spans"));
    let rebuilt = spans
        .iter()
        .find(|s| s.name == "tracker.fold.rebuild")
        .unwrap();
    assert_eq!(rebuilt.attributes["team"], "t-spans");
    // Neither span names an item.
    assert!(
        spans
            .iter()
            .all(|s| s.attributes.values().all(|v| v != "a1"))
    );
}
