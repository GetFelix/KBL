//! Poll, lag and error metrics, labelled by source only. The recorder is
//! shared by the process, so each test compares against what it read first.

use super::*;

/// The value of the exposition line `name{source="github"...}` holding every label in `has`.
fn read(name: &str, has: &[&str]) -> f64 {
    let text = kbl_metrics::install("kbl-ingest-test").render();
    text.lines()
        .filter(|l| l.starts_with(&format!("{name}{{")) && has.iter().all(|h| l.contains(h)))
        .find_map(|l| l.rsplit_once(' ')?.1.parse().ok())
        .unwrap_or(0.0)
}

#[tokio::test]
async fn a_poll_counts_its_result_and_the_lag_of_what_it_recorded() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    let ok = |n: &str| read(n, &["source=\"github\"", "result=\"ok\""]);
    let (polls, lagged) = (
        ok("kbl_ingest_polls_total"),
        read("kbl_ingest_lag_seconds_count", &["source=\"github\""]),
    );
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    assert_eq!(ok("kbl_ingest_polls_total") - polls, 1.0);
    assert_eq!(
        read("kbl_ingest_lag_seconds_count", &["source=\"github\""]) - lagged,
        1.0
    );
    assert!(read("kbl_ingest_lag_seconds_sum", &["source=\"github\""]) > 0.0);
    assert!(
        read(
            "kbl_ingest_last_success_timestamp_seconds",
            &["source=\"github\""]
        ) > 0.0
    );
}

#[tokio::test]
async fn a_failed_poll_is_an_error_for_its_source_and_names_no_repo() {
    let rig = rig();
    rig.gh.on("GET", LIST, status(500, &[]));
    let ingest = rig.ingest(json!([admit_rule()]));
    let labels = ["source=\"github\"", "kind=\"failed\""];
    let before = read("kbl_ingest_errors_total", &labels);
    assert!(ingest.poll(false).await.is_err());
    assert_eq!(read("kbl_ingest_errors_total", &labels) - before, 1.0);
    let text = kbl_metrics::install("kbl-ingest-test").render();
    for leaked in ["GetFelix", "felix", "KBL[bot]"] {
        assert!(
            !text.contains(leaked),
            "{leaked} is in the exposition:\n{text}"
        );
    }
}
