//! Upstream labels and milestone ride the import, reconcile keeps them in
//! step with an edit, and a rule's maps set the tier, type and area.

use kbl_core::work::{Target, Vocabulary, WorkRecord};

use super::*;

fn with_facts(number: u64, labels_: &[&str], milestone: Option<&str>) -> Value {
    issue(number, |v| {
        labels(v, labels_);
        if let Some(m) = milestone {
            v["milestone"] = json!({ "title": m });
        }
    })
}

fn raise(rig: &Rig, to: u32) {
    rig.tracker.person(WorkRecord::FormatRaised { to });
}

fn vocabulary(rig: &Rig) {
    rig.tracker.person(WorkRecord::RulesSet {
        target: Target {
            source: "github".into(),
            repo: "GetFelix/felix".into(),
        },
        root: None,
        text: None,
        checks: None,
        vocabulary: Some(Vocabulary {
            types: vec!["bug".into(), "perf".into()],
            areas: vec!["storage".into()],
            sizes: vec![],
        }),
    });
}

#[tokio::test]
async fn the_import_carries_the_issues_labels_and_milestone() {
    let rig = rig();
    raise(&rig, 19);
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![with_facts(812, &["agent-ok", "Perf"], Some("0.8"))]);
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    let it = &s.items[&rig.root(UP)];
    assert_eq!(it.labels, ["agent-ok", "Perf"]);
    assert_eq!(it.milestone.as_deref(), Some("0.8"));
}

#[tokio::test]
async fn reconcile_edits_a_changed_label_or_milestone_and_only_then() {
    let rig = rig();
    raise(&rig, 19);
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![with_facts(812, &["agent-ok"], Some("0.8"))]);
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("edited"), 0);
    rig.list(vec![at(
        with_facts(812, &["agent-ok", "perf"], Some("0.9")),
        "2027-01-16T00:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("edited"), 1);
    let s = rig.tracker.state();
    let it = &s.items[&rig.root(UP)];
    assert_eq!(it.labels, ["agent-ok", "perf"]);
    assert_eq!(it.milestone.as_deref(), Some("0.9"));
    // The milestone dropped upstream is cleared.
    rig.list(vec![at(
        with_facts(812, &["agent-ok", "perf"], None),
        "2027-01-17T00:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.state().items[&rig.root(UP)].milestone, None);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("edited"), 2);
}

#[tokio::test]
async fn a_log_below_format_19_imports_without_facts_and_catches_up_after_the_raise() {
    let rig = rig();
    raise(&rig, 18);
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![with_facts(812, &["agent-ok", "perf"], Some("0.8"))]);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("imported"), 1);
    assert!(rig.tracker.state().items[&rig.root(UP)].labels.is_empty());
    raise(&rig, 19);
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    let it = &s.items[&rig.root(UP)];
    assert_eq!(it.labels, ["agent-ok", "perf"]);
    assert_eq!(it.milestone.as_deref(), Some("0.8"));
}

#[tokio::test]
async fn a_rules_maps_set_tier_type_and_area_and_stay_inside_the_vocabulary() {
    let rig = rig();
    raise(&rig, 19);
    vocabulary(&rig);
    let mut rule = admit_rule();
    rule["milestone_tiers"] = json!({ "0.8": 0 });
    rule["label_types"] = json!({ "perf": "perf", "regression": "bug" });
    rule["label_areas"] = json!({ "disk": "storage", "ui": "dashboard" });
    let ingest = rig.ingest(json!([rule]));
    rig.list(vec![
        with_facts(812, &["agent-ok", "Perf", "disk"], Some("0.8")),
        with_facts(813, &["agent-ok", "ui"], None),
    ]);
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    let it = &s.items[&rig.root(UP)];
    // The milestone's tier beats the default of its p1 label, and the labels map to class.
    assert_eq!(it.tier, Some(0));
    assert_eq!(it.class.item_type.as_ref().unwrap().value, "perf");
    assert_eq!(it.class.area.as_ref().unwrap().value, "storage");
    // "dashboard" is not in the vocabulary: the tracker refuses it and the import stands.
    let other = &s.items[&rig.root("GetFelix/felix#813")];
    assert!(other.class.is_empty());
}

#[test]
fn a_milestone_tier_outside_the_tiers_is_a_config_error() {
    let mut rule = admit_rule();
    rule["milestone_tiers"] = json!({ "0.8": 9 });
    let err = Config::parse(
        &json!({
            "tracker": { "url": "http://t", "token_env": "T" },
            "github": { "token_env": "G" },
            "rules": [rule],
        })
        .to_string(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("tier"), "{err}");
}

#[tokio::test]
async fn a_value_outside_the_vocabulary_costs_only_itself() {
    let rig = rig();
    raise(&rig, 19);
    vocabulary(&rig);
    let mut rule = admit_rule();
    rule["label_types"] = json!({ "perf": "perf" });
    rule["label_areas"] = json!({ "ui": "dashboard" });
    let ingest = rig.ingest(json!([rule]));
    rig.list(vec![with_facts(812, &["agent-ok", "perf", "ui"], None)]);
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    let it = &s.items[&rig.root(UP)];
    assert_eq!(it.class.item_type.as_ref().unwrap().value, "perf");
    assert!(it.class.area.is_none());
}

#[tokio::test]
async fn a_closed_item_is_not_edited_again_every_poll() {
    let rig = rig();
    raise(&rig, 19);
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![with_facts(812, &["agent-ok"], Some("0.8"))]);
    ingest.poll(false).await.unwrap();
    assert!(rig.tracker.try_person(WorkRecord::Closed {
        item: rig.root(UP),
        why: kbl_core::work::CloseWhy::Dismissed,
    }));
    // A listing that lags the log still shows the root open.
    rig.tracker.roots_show_closed_as_open();
    rig.list(vec![at(
        with_facts(812, &["agent-ok", "perf"], Some("0.9")),
        "2027-01-16T00:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    let refused = rig.tracker.refused();
    for _ in 0..3 {
        ingest.poll(false).await.unwrap();
    }
    assert_eq!(rig.tracker.count("edited"), 0);
    assert_eq!(rig.tracker.refused(), refused);
}
