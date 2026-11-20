//! The triage acceptance policy: the tracker accepts a finished triage for a
//! rule, a crash before it does is finished by the sweep, and a person's
//! decision always wins.

use kbl_core::work::{Target, Vocabulary, WorkKind, WorkRecord};

use super::*;
use crate::policy::TriagePolicy;
use crate::tracker::tools::TriageAnswer;

const RULES: &str = r#"{"rules": [{
    "name": "routine",
    "tiers": [2, 3],
    "deny_areas": ["storage"],
    "deny_needs": ["safety-review"]
}]}"#;

async fn tracker(fake: &Fake, policy: Option<&str>) -> Tracker<Fake> {
    let triage_policy =
        policy.map_or_else(TriagePolicy::default, |p| TriagePolicy::parse(p).unwrap());
    start_with(
        fake,
        Settings {
            triage_policy,
            ..settings()
        },
    )
    .await
}

async fn person(t: &Tracker<Fake>, record: WorkRecord) {
    t.person_record(key("gab"), record).await.unwrap();
}

/// An imported root nobody accepted, with the triage item ingest posts for it,
/// in a repo with a vocabulary, on a log raised to the policy's format.
async fn imported(t: &Tracker<Fake>, id: &str) {
    person(
        t,
        WorkRecord::FormatRaised {
            to: kbl_core::work::FORMAT,
        },
    )
    .await;
    person(
        t,
        WorkRecord::RulesSet {
            target: Target {
                source: "github".into(),
                repo: "GetFelix/felix".into(),
            },
            root: None,
            text: None,
            checks: None,
            vocabulary: Some(Vocabulary {
                types: vec![],
                areas: vec!["docs".into(), "storage".into()],
                sizes: vec![],
                needs: vec!["safety-review".into()],
            }),
        },
    )
    .await;
    person(
        t,
        WorkRecord::Imported {
            item: item(id),
            source: "github".into(),
            upstream_id: format!("GetFelix/felix#{id}"),
            title: format!("fix {id}"),
            labels: vec![],
            milestone: None,
        },
    )
    .await;
    t.ingest_record(
        key("ingest"),
        WorkRecord::Proposed {
            item: item(&format!("{id}-t")),
            parent: Some(item(id)),
            key: key("triage"),
            work: WorkKind::Triage,
            title: "Triage".into(),
            review: None,
        },
    )
    .await
    .unwrap();
}

fn answer(tier: u8, area: &str, needs: &[&str]) -> TriageAnswer {
    TriageAnswer {
        tier,
        why: "a docs fix".into(),
        duplicate_of: None,
        unclear: None,
        depends_on: vec![],
        related: vec![],
        split: vec![],
        item_type: None,
        area: Some(area.into()),
        size: None,
        needs: Some(needs.iter().map(|n| (*n).to_owned()).collect()),
    }
}

/// w1 claims the triage item, answers it and says it is done.
async fn triage_and_finish(t: &Tracker<Fake>, id: &str, a: TriageAnswer) {
    let only_triage = Filter::parse(&["triage".to_string()], &[], None, &[]).unwrap();
    let reply = t
        .next_filtered(&agent("w1"), Duration::ZERO, &[], &only_triage)
        .await
        .unwrap();
    let triage_item = format!("{id}-t");
    assert_eq!(claimed_id(&reply), Some(triage_item.clone()));
    t.triage(&agent("w1"), &item(&triage_item), a)
        .await
        .unwrap();
    t.complete(&agent("w1"), &item(&triage_item), None, Vec::new())
        .await
        .unwrap();
}

fn root(t: &Tracker<Fake>, id: &str) -> kbl_core::work_fold::Item {
    t.0.core.try_lock().unwrap().state.items[&item(id)].clone()
}

fn accepts(fake: &Fake) -> usize {
    fake.kinds().iter().filter(|k| *k == "accepted").count()
}

#[tokio::test]
async fn with_no_policy_every_triage_waits_for_a_person() {
    let fake = Fake::default();
    let t = tracker(&fake, None).await;
    imported(&t, "a1").await;
    triage_and_finish(&t, "a1", answer(2, "docs", &[])).await;
    t.sweep().await;
    assert!(!root(&t, "a1").accepted);
    assert_eq!(accepts(&fake), 0);
    // Until a person accepts, nobody is handed the root's fix.
    assert_eq!(claimed_id(&claim(&t, "w2").await), None);
}

#[tokio::test]
async fn a_matching_rule_accepts_when_triage_finishes_and_the_item_is_claimable() {
    let fake = Fake::default();
    let t = tracker(&fake, Some(RULES)).await;
    imported(&t, "a1").await;
    triage_and_finish(&t, "a1", answer(2, "docs", &[])).await;
    let it = root(&t, "a1");
    assert_eq!(it.admitted_by.as_deref(), Some("routine"));
    assert!(it.accepted && !it.accepted_by_person);
    // The record is the tracker's, with the rule on it.
    let last = fake
        .log()
        .into_iter()
        .rev()
        .find(|l| l.record.kind() == "accepted")
        .unwrap();
    assert_eq!(last.actor, Actor::Tracker);
    assert!(
        matches!(&last.record, WorkRecord::Accepted { admitted_by: Some(r), .. } if r == "routine")
    );
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
    // A sweep and a restart write nothing more.
    t.sweep().await;
    let t = tracker(&fake, Some(RULES)).await;
    t.sweep().await;
    assert_eq!(accepts(&fake), 1);
}

#[tokio::test]
async fn a_triage_a_rule_does_not_match_stays_in_needs_you() {
    for (id, a) in [
        ("a1", answer(2, "storage", &[])),
        ("b1", answer(2, "docs", &["safety-review"])),
        ("c1", answer(0, "docs", &[])),
    ] {
        let fake = Fake::default();
        let t = tracker(&fake, Some(RULES)).await;
        imported(&t, id).await;
        triage_and_finish(&t, id, a).await;
        t.sweep().await;
        let it = root(&t, id);
        assert!(!it.accepted, "{id}");
        assert!(it.proposal.is_some(), "{id}: the answer waits for a person");
        assert_eq!(accepts(&fake), 0, "{id}");
    }
}

#[tokio::test]
async fn a_crash_between_triage_done_and_the_acceptance_is_finished_by_the_sweep() {
    let fake = Fake::default();
    let t = tracker(&fake, Some(RULES)).await;
    imported(&t, "a1").await;
    let only_triage = Filter::parse(&["triage".to_string()], &[], None, &[]).unwrap();
    t.next_filtered(&agent("w1"), Duration::ZERO, &[], &only_triage)
        .await
        .unwrap();
    t.triage(&agent("w1"), &item("a1-t"), answer(2, "docs", &[]))
        .await
        .unwrap();
    // The `done` lands, the acceptance after it does not.
    fake.fail_publishes_to_after(WORK_EVENTS, 1);
    let _ = t
        .complete(&agent("w1"), &item("a1-t"), None, Vec::new())
        .await;
    fake.lift_publish_budget();
    assert_eq!(accepts(&fake), 0, "the crash left it unaccepted");
    drop(t);
    // The restart sweeps once on its own, which finishes it.
    let t = tracker(&fake, Some(RULES)).await;
    assert_eq!(root(&t, "a1").admitted_by.as_deref(), Some("routine"));
    t.sweep().await;
    assert_eq!(accepts(&fake), 1, "finished once");
}

#[tokio::test]
async fn a_person_who_got_there_first_or_took_it_back_always_wins() {
    // Accepted by a person before triage finished: the rule leaves it.
    let fake = Fake::default();
    let t = tracker(&fake, Some(RULES)).await;
    imported(&t, "a1").await;
    person(
        &t,
        WorkRecord::Accepted {
            item: item("a1"),
            tier: Some(0),
            triage_skipped: None,
            admitted_by: None,
        },
    )
    .await;
    triage_and_finish(&t, "a1", answer(2, "docs", &[])).await;
    let it = root(&t, "a1");
    assert!(it.accepted_by_person && it.admitted_by.is_none());
    assert_eq!(it.tier, Some(0));

    // Taken back after the rule accepted: back to Triage, and it stays there.
    let fake = Fake::default();
    let t = tracker(&fake, Some(RULES)).await;
    imported(&t, "b1").await;
    triage_and_finish(&t, "b1", answer(2, "docs", &[])).await;
    assert!(root(&t, "b1").accepted);
    person(&t, WorkRecord::AdmissionUndone { item: item("b1") }).await;
    for _ in 0..3 {
        t.sweep().await;
    }
    let it = root(&t, "b1");
    assert!(!it.accepted && it.policy_held);
    assert_eq!(claimed_id(&claim(&t, "w2").await), None);
    assert_eq!(accepts(&fake), 1, "the rule did not accept again");
    // The person's own accept stands, and survives a restart.
    person(
        &t,
        WorkRecord::Accepted {
            item: item("b1"),
            tier: None,
            triage_skipped: None,
            admitted_by: None,
        },
    )
    .await;
    let t = tracker(&fake, Some(RULES)).await;
    assert!(root(&t, "b1").accepted_by_person);
}

#[tokio::test]
async fn a_log_below_the_format_gets_no_policy_acceptance() {
    let fake = Fake::default();
    let t = tracker(&fake, Some(RULES)).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 24 })
        .await
        .unwrap();
    person(
        &t,
        WorkRecord::Imported {
            item: item("a1"),
            source: "github".into(),
            upstream_id: "GetFelix/felix#a1".into(),
            title: "t".into(),
            labels: vec![],
            milestone: None,
        },
    )
    .await;
    t.ingest_record(
        key("ingest"),
        WorkRecord::Proposed {
            item: item("a1-t"),
            parent: Some(item("a1")),
            key: key("triage"),
            work: WorkKind::Triage,
            title: "Triage".into(),
            review: None,
        },
    )
    .await
    .unwrap();
    let mut plain = answer(2, "docs", &[]);
    (plain.area, plain.needs) = (None, None);
    triage_and_finish(&t, "a1", plain).await;
    t.sweep().await;
    assert!(!root(&t, "a1").accepted);
    assert_eq!(accepts(&fake), 0);
    // Raising the format does not sweep up a triage that was already waiting for a person.
    person(
        &t,
        WorkRecord::FormatRaised {
            to: kbl_core::work::FORMAT,
        },
    )
    .await;
    t.sweep().await;
    assert!(!root(&t, "a1").accepted);
    assert_eq!(accepts(&fake), 0);
}

#[tokio::test]
async fn ingest_cannot_write_a_policy_acceptance() {
    let fake = Fake::default();
    // No policy runs, and the triage is finished and plain: only the route's refusal stops it.
    let t = tracker(&fake, None).await;
    imported(&t, "a1").await;
    triage_and_finish(&t, "a1", answer(2, "docs", &[])).await;
    let err = t
        .ingest_record(
            key("ingest"),
            WorkRecord::Accepted {
                item: item("a1"),
                tier: None,
                triage_skipped: None,
                admitted_by: Some("routine".into()),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Invalid(_) | Error::Refused { .. }),
        "{err:?}"
    );
    assert!(!root(&t, "a1").accepted);
}
