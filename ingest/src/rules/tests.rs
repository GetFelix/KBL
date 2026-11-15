use serde_json::json;

use super::*;
use crate::config::Config;
use crate::source::IssueRef;

fn rules(v: serde_json::Value) -> Vec<Rule> {
    Config::parse(
        &json!({
            "tracker": { "url": "http://t", "token_env": "T" },
            "github": { "token_env": "G" },
            "rules": v,
        })
        .to_string(),
    )
    .unwrap()
    .rules
}

fn issue(labels: &[&str]) -> Issue {
    Issue {
        id: IssueRef::new("GetFelix/felix", 1),
        title: "t".into(),
        body: String::new(),
        closed: None,
        labels: labels.iter().map(|s| (*s).to_owned()).collect(),
        milestone: Some("0.8".into()),
        assignees: vec!["carol".into()],
        kind: Some("Bug".into()),
        author: "alice".into(),
        url: String::new(),
        updated_at: String::new(),
    }
}

#[test]
fn every_part_of_a_filter_must_hold() {
    let r = &rules(json!([{
        "name": "r", "repo": "GetFelix/felix", "mode": "admit",
        "labels_all": ["agent-ok", "bug"], "labels_any": ["p0", "p1"],
        "labels_excluded": ["wontfix"], "milestone": "0.8", "assignee": "Carol", "type": "bug"
    }]))[0];
    assert!(r.matches(&issue(&["agent-ok", "bug", "p1"])));
    assert!(r.matches(&issue(&["AGENT-OK", "Bug", "p0"])));
    assert!(!r.matches(&issue(&["agent-ok", "p1"])));
    assert!(!r.matches(&issue(&["agent-ok", "bug"])));
    assert!(!r.matches(&issue(&["agent-ok", "bug", "p1", "wontfix"])));
    let mut other = issue(&["agent-ok", "bug", "p1"]);
    other.milestone = Some("0.9".into());
    assert!(!r.matches(&other));
    other = issue(&["agent-ok", "bug", "p1"]);
    other.assignees.clear();
    assert!(!r.matches(&other));
    other = issue(&["agent-ok", "bug", "p1"]);
    other.id.repo = "GetFelix/other".into();
    assert!(!r.matches(&other));
    // The filter alone ignores the repository.
    assert!(r.filter(&other));
}

#[test]
fn the_first_matching_rule_wins_and_the_lowest_tier_label_sets_the_tier() {
    let rs = rules(json!([
        { "name": "a", "repo": "GetFelix/felix", "mode": "admit", "labels_all": ["agent-ok"],
          "priority_labels": { "p0": 0, "p1": 1 }, "default_tier": 3 },
        { "name": "b", "repo": "GetFelix/felix", "mode": "propose" }
    ]));
    let hit = first_match(&rs, &issue(&["agent-ok", "p1", "p0"])).unwrap();
    assert_eq!(hit.name, "a");
    assert_eq!(hit.tier_for(&issue(&["agent-ok", "p1", "p0"])), 0);
    assert_eq!(hit.tier_for(&issue(&["agent-ok"])), 3);
    assert_eq!(first_match(&rs, &issue(&["bug"])).unwrap().name, "b");
}
