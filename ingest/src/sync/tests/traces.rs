//! A poll is one span with the source kind and result, and nothing from the
//! issues it read.

use super::*;

#[tokio::test]
async fn a_poll_is_one_span_per_source_and_names_no_issue() {
    let capture = kbl_trace::capture();
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    rig.gh.on("GET", LIST, status(500, &[]));
    assert!(ingest.poll(true).await.is_err());

    let spans = capture.finished();
    let polls: Vec<_> = spans.iter().filter(|s| s.name == "ingest.poll").collect();
    assert_eq!(polls.len(), 2, "{spans:#?}");
    assert_eq!(polls[0].attributes["source"], "github");
    assert_eq!(polls[0].attributes["outcome"], "ok");
    assert_eq!(polls[0].attributes["reconcile"], "false");
    assert_eq!(polls[1].attributes["outcome"], "failed");
    assert_eq!(polls[1].attributes["reconcile"], "true");
    for s in &spans {
        for v in s.attributes.values() {
            assert!(!v.contains("812") && !v.contains("GetFelix"), "{s:#?}");
        }
    }
}
