//! Upstream labels and milestone on an item, triage's class and its
//! vocabulary, and a person's override (format 19).

use super::*;
use crate::work::Vocabulary;

fn import(item: &str, labels: &[&str], milestone: Option<&str>) -> WorkRecord {
    WorkRecord::Imported {
        item: id(item),
        source: "github".into(),
        upstream_id: format!("GetFelix/felix#{item}"),
        title: format!("bug {item}"),
        labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        milestone: milestone.map(str::to_owned),
    }
}

fn vocab(types: &[&str], areas: &[&str]) -> WorkRecord {
    WorkRecord::RulesSet {
        target: Target {
            source: "github".into(),
            repo: "GetFelix/felix".into(),
        },
        root: None,
        text: None,
        checks: None,
        vocabulary: Some(Vocabulary {
            types: types.iter().map(|t| (*t).to_owned()).collect(),
            areas: areas.iter().map(|t| (*t).to_owned()).collect(),
            sizes: vec![],
        }),
    }
}

fn triaged(item: &str, t: Option<&str>, a: Option<&str>, s: Option<&str>) -> WorkRecord {
    WorkRecord::Triaged {
        item: id(item),
        tier: 1,
        why: "fits".into(),
        duplicate_of: None,
        unclear: None,
        depends_on: vec![],
        split: vec![],
        item_type: t.map(str::to_owned),
        area: a.map(str::to_owned),
        size: s.map(str::to_owned),
        related: vec![],
    }
}

fn classed(item: &str, t: Option<&str>, a: Option<&str>, s: Option<&str>) -> WorkRecord {
    WorkRecord::Edited {
        item: id(item),
        title: None,
        tier: None,
        labels: None,
        milestone: None,
        item_type: t.map(str::to_owned),
        area: a.map(str::to_owned),
        size: s.map(str::to_owned),
        depends_on: None,
        split: None,
        proposal: None,
    }
}

fn facts(item: &str, labels: Option<&[&str]>, milestone: Option<&str>) -> WorkRecord {
    WorkRecord::Edited {
        item: id(item),
        title: None,
        tier: None,
        labels: labels.map(|l| l.iter().map(|x| (*x).to_owned()).collect()),
        milestone: milestone.map(str::to_owned),
        item_type: None,
        area: None,
        size: None,
        depends_on: None,
        split: None,
        proposal: None,
    }
}

fn at_19() -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 19 });
    l.person(vocab(&["bug", "perf"], &["storage"]));
    l
}

fn tag(it: &Item, dim: &str) -> Option<(String, ClassBy)> {
    let t = match dim {
        "type" => &it.class.item_type,
        "area" => &it.class.area,
        _ => &it.class.size,
    };
    t.as_ref().map(|t| (t.value.clone(), t.by))
}

#[test]
fn an_import_carries_cleaned_labels_and_a_milestone() {
    let mut l = at_19();
    let long = "x".repeat(200);
    l.tracker(import(
        "r1",
        &["Perf", "perf", " ", &long, "docs"],
        Some(" 0.8 "),
    ));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    let it = &s.items[&id("r1")];
    // A repeat without regard to case and an empty label are dropped; a long one is cut.
    assert_eq!(it.labels.len(), 3);
    assert_eq!(it.labels[0], "Perf");
    assert_eq!(it.labels[1].len(), crate::work::MAX_LABEL_LEN);
    assert_eq!(it.milestone.as_deref(), Some("0.8"));
}

#[test]
fn only_the_tracker_edits_upstream_facts_and_an_edit_replaces_them() {
    let mut l = at_19();
    l.tracker(import("r1", &["perf"], Some("0.8")));
    l.person(facts("r1", Some(&["x"]), None));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    l.0.pop();
    l.tracker(facts("r1", Some(&["docs", "P1"]), Some("0.9")));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].labels, ["docs", "P1"]);
    assert_eq!(s.items[&id("r1")].milestone.as_deref(), Some("0.9"));
    // An empty milestone clears it, and the same facts again change nothing.
    l.tracker(facts("r1", None, Some("")));
    assert_eq!(fold(&l.0).items[&id("r1")].milestone, None);
    l.tracker(facts("r1", None, Some("")));
    assert_eq!(last_rejection(&l), Some(Reject::NoChange));
}

#[test]
fn an_edit_that_mixes_upstream_facts_with_a_class_or_a_title_is_refused() {
    let mut l = at_19();
    l.tracker(import("r1", &[], None));
    let mut mixed = facts("r1", Some(&["a"]), None);
    if let WorkRecord::Edited { item_type, .. } = &mut mixed {
        *item_type = Some("bug".into());
    }
    l.tracker(mixed);
    assert_eq!(last_rejection(&l), Some(Reject::BadClass));
    l.0.pop();
    let mut titled = facts("r1", Some(&["a"]), None);
    if let WorkRecord::Edited { title, .. } = &mut titled {
        *title = Some("t".into());
    }
    l.person(titled);
    assert_eq!(last_rejection(&l), Some(Reject::BadClass));
}

#[test]
fn triage_classifies_from_the_vocabulary_and_a_value_outside_it_is_refused() {
    let mut l = at_19();
    l.tracker(import("r1", &[], None));
    l.person(triaged("r1", Some("Perf"), Some("storage"), Some("m")));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    // Stored as the vocabulary spells it, sizes default to S, M and L.
    let it = &s.items[&id("r1")];
    assert_eq!(tag(it, "type"), Some(("perf".into(), ClassBy::Triage)));
    assert_eq!(tag(it, "size"), Some(("M".into(), ClassBy::Triage)));
    // One bad value refuses the whole answer, so no half of it lands.
    l.person(triaged("r1", Some("bug"), Some("kernel"), None));
    assert_eq!(last_rejection(&l), Some(Reject::BadClass));
    assert_eq!(
        tag(&fold(&l.0).items[&id("r1")], "type"),
        Some(("perf".into(), ClassBy::Triage))
    );
    l.person(triaged("r1", None, None, Some("XL")));
    assert_eq!(last_rejection(&l), Some(Reject::BadClass));
}

#[test]
fn without_a_vocabulary_no_type_or_area_is_accepted() {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 19 });
    l.tracker(import("r1", &[], None));
    l.person(triaged("r1", Some("bug"), None, None));
    assert_eq!(last_rejection(&l), Some(Reject::BadClass));
    l.0.pop();
    l.person(triaged("r1", None, None, Some("S")));
    assert!(fold(&l.0).rejections.is_empty());
}

#[test]
fn a_vocabulary_with_a_repeat_or_a_bad_value_is_refused() {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 19 });
    l.person(vocab(&["bug", "BUG"], &[]));
    assert_eq!(last_rejection(&l), Some(Reject::BadClass));
    l.person(vocab(&["has space"], &[]));
    assert_eq!(last_rejection(&l), Some(Reject::BadClass));
}

#[test]
fn a_persons_choice_outlasts_triage_and_an_intake_rule() {
    let mut l = at_19();
    l.tracker(import("r1", &[], None));
    l.tracker(classed("r1", Some("bug"), None, None));
    assert_eq!(
        tag(&fold(&l.0).items[&id("r1")], "type"),
        Some(("bug".into(), ClassBy::Rule))
    );
    // Triage's call replaces a rule's mapping.
    l.person(triaged("r1", Some("perf"), Some("storage"), None));
    assert_eq!(
        tag(&fold(&l.0).items[&id("r1")], "type"),
        Some(("perf".into(), ClassBy::Triage))
    );
    // A person overrides it, and neither a later triage nor a rule changes it back.
    l.person(classed("r1", Some("bug"), None, None));
    l.person(triaged("r1", Some("perf"), Some("storage"), None));
    l.tracker(classed("r1", Some("perf"), None, None));
    let s = fold(&l.0);
    let it = &s.items[&id("r1")];
    assert_eq!(tag(it, "type"), Some(("bug".into(), ClassBy::Person)));
    assert_eq!(tag(it, "area"), Some(("storage".into(), ClassBy::Triage)));
    // Reconciling upstream facts leaves the class alone.
    l.tracker(facts("r1", Some(&["x"]), Some("1.0")));
    assert_eq!(
        tag(&fold(&l.0).items[&id("r1")], "type"),
        Some(("bug".into(), ClassBy::Person))
    );
}

#[test]
fn a_person_clears_their_own_value_and_an_agent_cannot_set_one() {
    let mut l = at_19();
    l.tracker(import("r1", &[], None));
    l.person(classed("r1", Some("bug"), None, None));
    l.person(classed("r1", Some(""), None, None));
    assert_eq!(tag(&fold(&l.0).items[&id("r1")], "type"), None);
    l.agent("w1", classed("r1", Some("bug"), None, None));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
}

#[test]
fn a_class_on_a_non_root_item_is_refused() {
    let mut l = at_19();
    l.tracker(import("r1", &[], None));
    l.tracker(proposed("r1-t", Some("r1"), "triage", WorkKind::Triage));
    assert_eq!(fold(&l.0).rejections, []);
    l.person(classed("r1-t", Some("bug"), None, None));
    assert_eq!(last_rejection(&l), Some(Reject::NotARoot));
    l.0.pop();
    l.person(triaged("r1-t", Some("bug"), None, None));
    assert_eq!(last_rejection(&l), Some(Reject::NotARoot));
    l.0.pop();
    l.person(classed("r1", Some("bug"), None, None));
    assert_eq!(last_rejection(&l), None);
}

#[test]
fn a_class_is_refused_below_format_19() {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 18 });
    l.tracker(import("r1", &[], None));
    l.person(classed("r1", Some("bug"), None, None));
    assert_eq!(last_rejection(&l), Some(Reject::Format));
    l.0.pop();
    l.tracker(import("r2", &["perf"], None));
    assert_eq!(last_rejection(&l), Some(Reject::Format));
    l.0.pop();
    l.person(vocab(&["bug"], &[]));
    assert_eq!(last_rejection(&l), Some(Reject::Format));
}

#[test]
fn a_vocabulary_keeps_the_scopes_text_and_a_text_keeps_the_vocabulary() {
    let mut l = at_19();
    let text = |t: &str| WorkRecord::RulesSet {
        target: Target {
            source: "github".into(),
            repo: "GetFelix/felix".into(),
        },
        root: None,
        text: Some(t.into()),
        checks: None,
        vocabulary: None,
    };
    l.person(text("use the lock"));
    l.tracker(import("r1", &[], None));
    let s = fold(&l.0);
    let rules = s.rules_for(&id("r1")).unwrap();
    assert_eq!(rules.text.as_deref(), Some("use the lock"));
    assert!(rules.vocabulary.is_some());
    l.person(vocab(&["bug"], &[]));
    let rules = fold(&l.0).rules_for(&id("r1")).unwrap();
    assert_eq!(rules.text.as_deref(), Some("use the lock"));
    assert_eq!(rules.vocabulary.unwrap().types, ["bug"]);
}
