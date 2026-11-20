//! Filtering by a root's milestone, upstream labels and class: a filtered
//! claim passes over the others without spending their ready records.

use kbl_core::work::{Vocabulary, WorkRecord};

use super::*;
use crate::tracker::selection::Facets;

async fn at_19(fake: &Fake) -> Tracker<Fake> {
    let t = start(fake).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 19 })
        .await
        .unwrap();
    t
}

/// Imports and accepts a root in GetFelix/felix with the given facts.
async fn root(t: &Tracker<Fake>, id: &str, milestone: Option<&str>, labels: &[&str]) {
    t.person_record(
        key("gab"),
        WorkRecord::Imported {
            item: item(id),
            source: "github".into(),
            upstream_id: format!("GetFelix/felix#{id}"),
            title: format!("fix {id}"),
            labels: labels.iter().map(|l| (*l).to_owned()).collect(),
            milestone: milestone.map(str::to_owned),
        },
    )
    .await
    .unwrap();
    t.person_record(
        key("gab"),
        WorkRecord::Accepted {
            item: item(id),
            tier: Some(2),
            triage_skipped: None,
        },
    )
    .await
    .unwrap();
}

fn facets(f: impl FnOnce(&mut Facets)) -> Filter {
    let mut x = Facets::default();
    f(&mut x);
    Filter::default().with_facets(x).unwrap()
}

fn ids(reply: &Value) -> Vec<&str> {
    reply["next"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["item"].as_str().unwrap())
        .collect()
}

async fn vocabulary(t: &Tracker<Fake>) {
    t.person_record(
        key("gab"),
        WorkRecord::RulesSet {
            target: kbl_core::work::Target {
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
                needs: vec![],
            }),
        },
    )
    .await
    .unwrap();
}

async fn classify(t: &Tracker<Fake>, id: &str, item_type: &str) -> Result<Value, Error> {
    t.person_record(
        key("gab"),
        WorkRecord::Edited {
            item: item(id),
            title: None,
            tier: None,
            labels: None,
            milestone: None,
            item_type: Some(item_type.into()),
            area: None,
            size: None,
            needs: None,
            depends_on: None,
            split: None,
            proposal: None,
        },
    )
    .await
}

#[tokio::test]
async fn a_milestone_filter_claims_only_that_milestone_and_spends_no_other_record() {
    let fake = Fake::default();
    let t = at_19(&fake).await;
    root(&t, "a1", Some("0.9"), &[]).await;
    root(&t, "b1", Some("0.8"), &[]).await;
    root(&t, "c1", None, &[]).await;
    let f = facets(|x| x.milestone = Some("0.8".into()));
    let reply = t
        .next_filtered(&agent("w1"), Duration::ZERO, &[], &f)
        .await
        .unwrap();
    assert_eq!(claimed_id(&reply).as_deref(), Some("b1"));
    // Nothing else matches: the others are passed over, not claimed, acked or nacked.
    let reply = t
        .next_filtered(&agent("w2"), Duration::ZERO, &[], &f)
        .await
        .unwrap();
    assert!(claimed_id(&reply).is_none());
    assert_eq!(
        fake.tries("work.ready.p2")
            .iter()
            .map(|(_, n)| n)
            .sum::<u32>(),
        0
    );
    assert!(!fake.calls().iter().any(|c| c.starts_with("nack")));
    // Without the filter the rest are still there.
    assert!(claimed_id(&claim(&t, "w2").await).is_some());
}

#[tokio::test]
async fn a_label_filter_matches_any_one_without_regard_to_case() {
    let fake = Fake::default();
    let t = at_19(&fake).await;
    root(&t, "a1", None, &["Perf", "P1"]).await;
    root(&t, "b1", None, &["docs"]).await;
    root(&t, "c1", None, &[]).await;
    let f = facets(|x| x.labels = vec!["nope".into(), "perf".into()]);
    let reply = t.peek(&agent("w1"), 10, &[], &f).await.unwrap();
    assert_eq!(ids(&reply), ["a1"]);
    assert_eq!(reply["next"][0]["labels"], json!(["Perf", "P1"]));
    let reply = t
        .next_filtered(&agent("w1"), Duration::ZERO, &[], &f)
        .await
        .unwrap();
    assert_eq!(claimed_id(&reply).as_deref(), Some("a1"));
    let other = facets(|x| x.labels = vec!["DOCS".into()]);
    let reply = t
        .next_filtered(&agent("w2"), Duration::ZERO, &[], &other)
        .await
        .unwrap();
    assert_eq!(claimed_id(&reply).as_deref(), Some("b1"));
}

#[tokio::test]
async fn a_children_work_is_filtered_by_its_roots_facts() {
    let fake = Fake::default();
    let t = at_19(&fake).await;
    root(&t, "a1", Some("0.8"), &[]).await;
    root(&t, "b1", Some("0.9"), &[]).await;
    t.ingest_record(
        key("ingest"),
        WorkRecord::Proposed {
            item: item("a1-t"),
            parent: Some(item("a1")),
            key: key("triage"),
            work: kbl_core::work::WorkKind::Triage,
            title: "Triage".into(),
            review: None,
        },
    )
    .await
    .unwrap();
    let f = facets(|x| x.milestone = Some("0.8".into()));
    let reply = t.peek(&agent("w1"), 10, &[], &f).await.unwrap();
    assert!(ids(&reply).iter().all(|i| i.starts_with("a1")), "{reply}");
    assert!(ids(&reply).contains(&"a1-t"));
}

#[tokio::test]
async fn a_class_filter_follows_a_persons_choice_and_the_vocabulary_bounds_it() {
    let fake = Fake::default();
    let t = at_19(&fake).await;
    vocabulary(&t).await;
    root(&t, "a1", None, &[]).await;
    root(&t, "b1", None, &[]).await;
    classify(&t, "a1", "perf").await.unwrap();
    // A value the team's vocabulary does not list is refused and changes nothing.
    let err = classify(&t, "b1", "chore").await.unwrap_err();
    assert!(
        matches!(err, Error::Refused { ref why } if why == "bad_class"),
        "{err:?}"
    );
    let f = facets(|x| x.item_type = Some("PERF".into()));
    let reply = t.peek(&agent("w1"), 10, &[], &f).await.unwrap();
    assert_eq!(ids(&reply), ["a1"]);
    assert_eq!(reply["next"][0]["type"], "perf");
    let none = facets(|x| x.item_type = Some("bug".into()));
    let reply = t.peek(&agent("w1"), 10, &[], &none).await.unwrap();
    assert!(ids(&reply).is_empty());
}

#[tokio::test]
async fn upstream_facts_are_written_by_ingest_and_not_by_a_person() {
    let fake = Fake::default();
    let t = at_19(&fake).await;
    root(&t, "a1", Some("0.8"), &["perf"]).await;
    let edit = |labels: Vec<&str>, milestone: &str| WorkRecord::Edited {
        item: item("a1"),
        title: None,
        tier: None,
        labels: Some(labels.into_iter().map(str::to_owned).collect()),
        milestone: Some(milestone.into()),
        item_type: None,
        area: None,
        size: None,
        needs: None,
        depends_on: None,
        split: None,
        proposal: None,
    };
    assert!(
        t.person_record(key("gab"), edit(vec!["x"], "0.9"))
            .await
            .is_err()
    );
    t.ingest_record(key("ingest"), edit(vec!["docs"], "0.9"))
        .await
        .unwrap();
    let f = facets(|x| x.milestone = Some("0.9".into()));
    let reply = t.peek(&agent("w1"), 10, &[], &f).await.unwrap();
    assert_eq!(ids(&reply), ["a1"]);
    assert_eq!(reply["next"][0]["labels"], json!(["docs"]));
}

#[tokio::test]
async fn a_facet_is_refused_below_format_19() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let err = t
        .person_record(
            key("gab"),
            WorkRecord::Imported {
                item: item("a1"),
                source: "github".into(),
                upstream_id: "GetFelix/felix#1".into(),
                title: "t".into(),
                labels: vec!["perf".into()],
                milestone: None,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Format(_)), "{err:?}");
}

#[tokio::test]
async fn a_persons_class_does_not_make_the_agents_repeated_answer_a_new_one() {
    let fake = Fake::default();
    let t = at_19(&fake).await;
    vocabulary(&t).await;
    root(&t, "a1", None, &[]).await;
    t.ingest_record(
        key("ingest"),
        WorkRecord::Proposed {
            item: item("a1-t"),
            parent: Some(item("a1")),
            key: key("triage"),
            work: kbl_core::work::WorkKind::Triage,
            title: "Triage".into(),
            review: None,
        },
    )
    .await
    .unwrap();
    let only_triage = Filter::parse(&["triage".to_string()], &[], None, &[]).unwrap();
    let reply = t
        .next_filtered(&agent("w1"), Duration::ZERO, &[], &only_triage)
        .await
        .unwrap();
    assert_eq!(claimed_id(&reply).as_deref(), Some("a1-t"));
    let answer = || super::super::tools::TriageAnswer {
        tier: 1,
        why: "perf".into(),
        duplicate_of: None,
        unclear: None,
        depends_on: vec![],
        related: vec![],
        split: vec![],
        item_type: Some("perf".into()),
        area: None,
        size: None,
        needs: None,
    };
    t.triage(&agent("w1"), &item("a1-t"), answer())
        .await
        .unwrap();
    classify(&t, "a1", "bug").await.unwrap();
    let before = fake.kinds().len();
    t.triage(&agent("w1"), &item("a1-t"), answer())
        .await
        .unwrap();
    assert_eq!(fake.kinds().len(), before);
}
