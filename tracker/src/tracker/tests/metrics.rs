//! The tracker's claim, hand-back and fold metrics. The recorder is shared by
//! every test in the process, so each test reads only its own `team` label.

use super::*;

/// The value of the exposition line that starts with `name{...}` and holds every label in `has`.
fn value(text: &str, name: &str, has: &[&str]) -> Option<f64> {
    text.lines()
        .filter(|l| l.starts_with(&format!("{name}{{")) && has.iter().all(|h| l.contains(h)))
        .find_map(|l| l.rsplit_once(' ')?.1.parse().ok())
}

fn team_settings(team: &str) -> Settings {
    Settings {
        team: team.into(),
        ..settings()
    }
}

#[tokio::test]
async fn claims_releases_give_ups_and_completions_are_counted_per_team() {
    let exporter = kbl_metrics::install("kbl-tracker-test");
    let fake = Fake::default();
    let t = start_with(&fake, team_settings("t-claims")).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    t.release(&agent("w1"), &item("a1"), None, None, None, Vec::new())
        .await
        .unwrap();
    claim(&t, "w1").await;
    t.give_up(&agent("w1"), &item("a1"), None, "stuck".into(), Vec::new())
        .await
        .unwrap();
    seed(&t, "a2", 2).await;
    let second = claim(&t, "w1").await;
    assert_eq!(second["claimed"], true);
    let held = item(second["item"]["id"].as_str().unwrap());
    t.complete(&agent("w1"), &held, None, Vec::new())
        .await
        .unwrap();
    assert_eq!(claim(&t, "w1").await["claimed"], false);

    let text = exporter.render();
    let team = "team=\"t-claims\"";
    let v = |name: &str, more: &[&str]| {
        let mut has = vec![team];
        has.extend_from_slice(more);
        value(&text, name, &has)
    };
    assert_eq!(
        v("kbl_tracker_claims_total", &["outcome=\"claimed\""]),
        Some(3.0),
        "{text}"
    );
    assert_eq!(
        v("kbl_tracker_claims_total", &["outcome=\"empty\""]),
        Some(1.0),
        "{text}"
    );
    assert_eq!(v("kbl_tracker_releases_total", &[]), Some(1.0), "{text}");
    assert_eq!(v("kbl_tracker_give_ups_total", &[]), Some(1.0), "{text}");
    assert_eq!(v("kbl_tracker_completions_total", &[]), Some(1.0), "{text}");
    assert_eq!(
        v("kbl_tracker_claim_seconds_count", &["outcome=\"claimed\""]),
        Some(3.0),
        "{text}"
    );
}

#[tokio::test]
async fn the_fold_gauges_follow_the_stream_and_a_rebuild_is_timed() {
    let exporter = kbl_metrics::install("kbl-tracker-test");
    let fake = Fake::default();
    let t = start_with(&fake, team_settings("t-fold")).await;
    seed(&t, "a1", 2).await;
    t.sample_fold().await;
    let text = exporter.render();
    let team = "team=\"t-fold\"";
    let applied = value(&text, "kbl_tracker_fold_applied_offset", &[team]).expect(&text);
    let tip = value(&text, "kbl_tracker_stream_tip_offset", &[team]).expect(&text);
    assert!(
        tip > applied,
        "the tip is the offset after the last record: {text}"
    );
    assert_eq!(
        value(&text, "kbl_tracker_fold_lag_records", &[team]),
        Some(0.0),
        "{text}"
    );

    t.lease().stale = true;
    t.begin().await.unwrap();
    let text = exporter.render();
    assert_eq!(
        value(&text, "kbl_tracker_fold_rebuilds_total", &[team]),
        Some(1.0),
        "{text}"
    );
    assert_eq!(
        value(&text, "kbl_tracker_fold_rebuild_seconds_count", &[team]),
        Some(1.0),
        "{text}"
    );
}
