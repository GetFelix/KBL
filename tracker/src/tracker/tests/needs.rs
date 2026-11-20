//! An agent is handed only work whose needs its declared capabilities cover.
//! A skipped item spends no ready record, and the rule holds across a restart.

use kbl_core::work::{Vocabulary, WorkRecord};

use super::*;

async fn at_20(fake: &Fake) -> Tracker<Fake> {
    let t = start(fake).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 20 })
        .await
        .unwrap();
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
                needs: vec!["azure".into(), "gpu".into(), "network".into()],
                ..Default::default()
            }),
        },
    )
    .await
    .unwrap();
    t
}

async fn root(t: &Tracker<Fake>, id: &str, needs: &[&str]) {
    t.person_record(
        key("gab"),
        WorkRecord::Imported {
            item: item(id),
            source: "github".into(),
            upstream_id: format!("GetFelix/felix#{id}"),
            title: format!("fix {id}"),
            labels: vec![],
            milestone: None,
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
    if !needs.is_empty() {
        set_needs(t, id, needs).await;
    }
}

async fn set_needs(t: &Tracker<Fake>, id: &str, needs: &[&str]) {
    t.person_record(
        key("gab"),
        WorkRecord::Edited {
            item: item(id),
            title: None,
            tier: None,
            labels: None,
            milestone: None,
            item_type: None,
            area: None,
            size: None,
            needs: Some(needs.iter().map(|n| (*n).to_owned()).collect()),
            depends_on: None,
            split: None,
            proposal: None,
        },
    )
    .await
    .unwrap();
}

fn can(caps: &[&str]) -> Filter {
    Filter::default()
        .with_capabilities(caps.iter().map(|c| (*c).to_owned()).collect())
        .unwrap()
}

async fn take(t: &Tracker<Fake>, who: &str, caps: &[&str]) -> Option<String> {
    let reply = t
        .next_filtered(&agent(who), Duration::ZERO, &[], &can(caps))
        .await
        .unwrap();
    claimed_id(&reply)
}

fn count(fake: &Fake, call: &str) -> usize {
    fake.calls().iter().filter(|c| c.starts_with(call)).count()
}

fn nack_total(fake: &Fake) -> u32 {
    fake.tries("work.ready.p2").iter().map(|(_, n)| n).sum()
}

#[tokio::test]
async fn an_agent_without_the_capability_is_never_handed_the_item_and_no_record_is_spent() {
    let fake = Fake::default();
    let t = at_20(&fake).await;
    root(&t, "a1", &["gpu"]).await;
    let polls = count(&fake, "poll");
    // A lone root: without a need the lane would be polled, and its record spent.
    for caps in [&[][..], &["azure"], &["network", "azure"]] {
        assert_eq!(take(&t, "w1", caps).await, None);
    }
    assert!(claimed_id(&claim(&t, "w1").await).is_none(), "plain next");
    assert_eq!(count(&fake, "poll"), polls);
    assert_eq!(count(&fake, "nack"), 0);
    assert_eq!(nack_total(&fake), 0);
    assert_eq!(fake.slot("work.ready.p2", 0), Slot::Pending);
    // An agent that declares the need, in any case, takes it.
    assert_eq!(take(&t, "w2", &["GPU"]).await.as_deref(), Some("a1"));
}

#[tokio::test]
async fn work_that_needs_several_things_needs_all_of_them() {
    let fake = Fake::default();
    let t = at_20(&fake).await;
    root(&t, "a1", &["gpu", "azure"]).await;
    assert_eq!(take(&t, "w1", &["gpu"]).await, None);
    assert_eq!(take(&t, "w1", &["azure", "network"]).await, None);
    assert_eq!(
        take(&t, "w1", &["azure", "gpu"]).await.as_deref(),
        Some("a1")
    );
}

#[tokio::test]
async fn other_work_is_still_handed_out_beside_an_item_that_is_skipped() {
    let fake = Fake::default();
    let t = at_20(&fake).await;
    root(&t, "a1", &["gpu"]).await;
    root(&t, "b1", &[]).await;
    let polls = count(&fake, "poll");
    assert_eq!(take(&t, "w1", &[]).await.as_deref(), Some("b1"));
    assert_eq!(take(&t, "w2", &[]).await, None);
    assert_eq!(count(&fake, "poll"), polls, "claimed from the state");
    assert_eq!(nack_total(&fake), 0);
    assert_eq!(take(&t, "w3", &["gpu"]).await.as_deref(), Some("a1"));
}

#[tokio::test]
async fn the_rule_holds_across_a_restart() {
    let fake = Fake::default();
    let t = at_20(&fake).await;
    root(&t, "a1", &["gpu"]).await;
    root(&t, "b1", &[]).await;
    drop(t);
    let t = start(&fake).await;
    assert_eq!(take(&t, "w1", &["azure"]).await.as_deref(), Some("b1"));
    assert_eq!(take(&t, "w2", &["azure"]).await, None);
    drop(t);
    let t = start(&fake).await;
    assert_eq!(take(&t, "w2", &[]).await, None);
    assert_eq!(take(&t, "w2", &["gpu"]).await.as_deref(), Some("a1"));
}

#[tokio::test]
async fn needs_set_after_the_record_was_published_still_hold_the_item_back() {
    let fake = Fake::default();
    let t = at_20(&fake).await;
    root(&t, "a1", &[]).await;
    assert_eq!(fake.ready_count("work.ready.p2"), 1);
    // The record is already published when the need arrives.
    set_needs(&t, "a1", &["network"]).await;
    assert_eq!(take(&t, "w1", &[]).await, None);
    assert_eq!(nack_total(&fake), 0);
    assert_eq!(take(&t, "w1", &["network"]).await.as_deref(), Some("a1"));
}

#[tokio::test]
async fn a_polled_record_whose_item_gained_a_need_is_not_claimed() {
    let fake = Fake::default();
    let t = at_20(&fake).await;
    root(&t, "a1", &[]).await;
    let record = fake.poll_sync("work.ready.p2", "w1").unwrap();
    set_needs(&t, "a1", &["gpu"]).await;
    let mut ops = Vec::new();
    let mut core = t.0.core.lock().await;
    let taken = t
        .claim_from(&mut core, &agent("w1"), 2, record, 0, &can(&[]), &mut ops)
        .await
        .unwrap();
    assert!(taken.is_none());
    assert!(!core.state.items[&item("a1")].hold.is_some());
}

#[tokio::test]
async fn a_peek_lists_only_what_the_caller_can_take_and_shows_the_needs() {
    let fake = Fake::default();
    let t = at_20(&fake).await;
    root(&t, "a1", &["gpu"]).await;
    root(&t, "b1", &[]).await;
    let ids = |r: &Value| -> Vec<(String, Vec<String>)> {
        r["next"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["item"].as_str().unwrap().to_owned(),
                    i["needs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|n| n.as_str().unwrap().to_owned())
                        .collect(),
                )
            })
            .collect()
    };
    let none = t.peek(&agent("w1"), 10, &[], &can(&[])).await.unwrap();
    assert_eq!(ids(&none), [("b1".to_owned(), vec![])]);
    let gpu = t.peek(&agent("w1"), 10, &[], &can(&["gpu"])).await.unwrap();
    let mut got = ids(&gpu);
    got.sort();
    assert_eq!(
        got,
        [
            ("a1".to_owned(), vec!["gpu".to_owned()]),
            ("b1".to_owned(), vec![])
        ]
    );
}

#[tokio::test]
async fn an_assigned_item_is_not_handed_to_an_agent_without_the_capability() {
    use kbl_core::work::Assignee;
    let fake = Fake::default();
    let t = at_20(&fake).await;
    root(&t, "a1", &["gpu"]).await;
    t.person_record(
        key("gab"),
        WorkRecord::Reassigned {
            item: item("a1"),
            to: Some(Assignee::Agent(key("w1"))),
        },
    )
    .await
    .unwrap();
    assert_eq!(take(&t, "w1", &[]).await, None);
    assert_eq!(take(&t, "w1", &["gpu"]).await.as_deref(), Some("a1"));
}

#[tokio::test]
async fn capabilities_are_bounded_and_plain() {
    assert!(
        Filter::default()
            .with_capabilities(vec!["has space".into()])
            .is_err()
    );
    assert!(
        Filter::default()
            .with_capabilities(vec!["gpu".into(); 33])
            .is_err()
    );
}

#[tokio::test]
async fn needs_are_refused_below_format_20() {
    let fake = Fake::default();
    let t = start(&fake).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 19 })
        .await
        .unwrap();
    root(&t, "a1", &[]).await;
    let err = t
        .person_record(
            key("gab"),
            WorkRecord::ResourceLifted {
                resource: "azure-vms".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Format(_)), "{err:?}");
}

fn answer(needs: Option<Vec<String>>) -> super::super::tools::TriageAnswer {
    super::super::tools::TriageAnswer {
        tier: 1,
        why: "needs a gpu".into(),
        duplicate_of: None,
        unclear: None,
        depends_on: vec![],
        related: vec![],
        split: vec![],
        item_type: None,
        area: None,
        size: None,
        needs,
    }
}

#[tokio::test]
async fn triage_sets_needs_and_a_repeat_is_a_retry_but_a_persons_needs_stand() {
    let fake = Fake::default();
    let t = at_20(&fake).await;
    root(&t, "a1", &[]).await;
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
    // The triage item itself needs nothing, whatever the root needs.
    let reply = t
        .next_filtered(
            &agent("w1"),
            Duration::ZERO,
            &[],
            &Filter::parse(&["triage".to_string()], &[], None, &[]).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(claimed_id(&reply).as_deref(), Some("a1-t"));
    let gpu = Some(vec!["gpu".to_owned()]);
    let bad = t
        .triage(
            &agent("w1"),
            &item("a1-t"),
            answer(Some(vec!["tpu".into()])),
        )
        .await;
    assert!(bad.is_err());
    t.triage(&agent("w1"), &item("a1-t"), answer(gpu.clone()))
        .await
        .unwrap();
    let need = |t: &Tracker<Fake>| {
        let core = t.0.core.try_lock().unwrap();
        core.state.items[&item("a1")].class.needs().to_vec()
    };
    assert_eq!(need(&t), ["gpu"]);
    let before = fake.kinds().len();
    t.triage(&agent("w1"), &item("a1-t"), answer(gpu.clone()))
        .await
        .unwrap();
    assert_eq!(fake.kinds().len(), before, "the same answer is a retry");
    set_needs(&t, "a1", &["azure"]).await;
    let before = fake.kinds().len();
    t.triage(&agent("w1"), &item("a1-t"), answer(gpu))
        .await
        .unwrap();
    assert_eq!(fake.kinds().len(), before, "a person's needs stand");
    assert_eq!(need(&t), ["azure"]);
}

#[tokio::test]
async fn lifting_a_resource_frees_every_item_blocked_on_it_and_survives_a_restart() {
    let fake = Fake::default();
    let t = at_20(&fake).await;
    for id in ["a1", "b1", "c1", "d1"] {
        root(&t, id, &[]).await;
    }
    let block = |id: &str, resource: Option<&str>, until_ms: Option<u64>| WorkRecord::BlockSet {
        item: item(id),
        until_ms,
        resource: resource.map(str::to_owned),
        reason: "no credit".into(),
    };
    for r in [
        block("a1", Some("azure-vms"), None),
        block("b1", Some("azure-vms"), None),
        block("c1", Some("gpu-pool"), None),
        block("d1", None, Some(now_ms() + 3_600_000)),
    ] {
        t.person_record(key("gab"), r).await.unwrap();
    }
    assert_eq!(take(&t, "w1", &[]).await, None);
    t.person_record(
        key("gab"),
        WorkRecord::ResourceLifted {
            resource: "azure-vms".into(),
        },
    )
    .await
    .unwrap();
    drop(t);
    let t = start(&fake).await;
    let mut got = vec![
        take(&t, "w1", &[]).await.unwrap(),
        take(&t, "w2", &[]).await.unwrap(),
    ];
    got.sort();
    assert_eq!(got, ["a1", "b1"]);
    // c1 waits on another resource and d1 on a time: neither was lifted.
    assert_eq!(take(&t, "w3", &[]).await, None);
}

#[tokio::test]
async fn a_lone_blocked_root_is_published_again_when_its_resource_is_lifted() {
    let fake = Fake::default();
    let t = at_20(&fake).await;
    root(&t, "a1", &[]).await;
    t.person_record(
        key("gab"),
        WorkRecord::BlockSet {
            item: item("a1"),
            until_ms: None,
            resource: Some("azure-vms".into()),
            reason: "no credit".into(),
        },
    )
    .await
    .unwrap();
    // The lane is polled and the record acked while the block holds.
    assert!(claimed_id(&claim(&t, "w1").await).is_none());
    t.person_record(
        key("gab"),
        WorkRecord::ResourceLifted {
            resource: "azure-vms".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
}
