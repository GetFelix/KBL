//! Poll, lag and error metrics, labelled by source only. Each test records
//! into a recorder of its own on its thread, because every sync test polls the
//! same source and would move a shared counter.

use super::*;

/// A recorder for this test's thread, and what it has rendered so far.
fn scope() -> (impl Fn() -> String, ::metrics::LocalRecorderGuard<'static>) {
    let (recorder, handle) = kbl_metrics::local_recorder();
    let guard = ::metrics::set_default_local_recorder(Box::leak(Box::new(recorder)));
    (move || handle.render(), guard)
}

/// The value of the exposition line `name{source="github"...}` holding every label in `has`.
fn read(text: &str, name: &str, has: &[&str]) -> f64 {
    text.lines()
        .filter(|l| l.starts_with(&format!("{name}{{")) && has.iter().all(|h| l.contains(h)))
        .find_map(|l| l.rsplit_once(' ')?.1.parse().ok())
        .unwrap_or(0.0)
}

#[tokio::test]
async fn a_poll_counts_its_result_and_the_lag_of_what_it_recorded() {
    let (text, _scope) = scope();
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    let github = ["source=\"github\""];
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    let ok = ["source=\"github\"", "result=\"ok\""];
    assert_eq!(read(&text(), "kbl_ingest_polls_total", &ok), 1.0);
    // The first sync reads the backlog, so it is not lag.
    assert_eq!(read(&text(), "kbl_ingest_lag_seconds_count", &github), 0.0);
    ingest.poll(false).await.unwrap();
    assert_eq!(read(&text(), "kbl_ingest_lag_seconds_count", &github), 1.0);
    assert!(read(&text(), "kbl_ingest_lag_seconds_sum", &github) > 0.0);
    assert!(
        read(
            &text(),
            "kbl_ingest_last_success_timestamp_seconds",
            &github
        ) > 0.0
    );
}

#[tokio::test]
async fn a_failed_poll_is_an_error_for_its_source_and_names_no_repo() {
    let (text, _scope) = scope();
    let rig = rig();
    rig.gh.on("GET", LIST, status(500, &[]));
    let ingest = rig.ingest(json!([admit_rule()]));
    let labels = ["source=\"github\"", "kind=\"failed\""];
    assert!(ingest.poll(false).await.is_err());
    let text = text();
    assert_eq!(read(&text, "kbl_ingest_errors_total", &labels), 1.0);
    for leaked in ["GetFelix", "felix", "KBL[bot]"] {
        assert!(
            !text.contains(leaked),
            "{leaked} is in the exposition:\n{text}"
        );
    }
}
