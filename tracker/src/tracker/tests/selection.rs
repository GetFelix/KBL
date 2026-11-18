//! Filters on `work_next`, the peek, and fairness across roots. A claim taken
//! from the state never spends a ready record on an item it passed over.

use kbl_core::work::WorkKind;

use super::*;

fn filter(kinds: &[&str], tiers: &[u8], repo: Option<&str>, exclude: &[&str]) -> Filter {
    let exclude: Vec<ItemId> = exclude.iter().map(|r| item(r)).collect();
    Filter::parse(
        &kinds.iter().map(|k| k.to_string()).collect::<Vec<_>>(),
        tiers,
        repo,
        &exclude,
    )
    .unwrap()
}

async fn next(t: &Tracker<Fake>, who: &str, f: &Filter) -> Option<String> {
    let reply = t
        .next_filtered(&agent(who), Duration::ZERO, &[], f)
        .await
        .unwrap();
    claimed_id(&reply)
}

async fn child(t: &Tracker<Fake>, id: &str, parent: &str, work: WorkKind, tier: u8) {
    t.person_record(
        key("gab"),
        WorkRecord::Proposed {
            item: item(id),
            parent: Some(item(parent)),
            key: key(id),
            work,
            title: format!("{work:?} {id}"),
            review: None,
        },
    )
    .await
    .unwrap();
    t.person_record(
        key("gab"),
        WorkRecord::Accepted {
            item: item(id),
            tier: Some(tier),
        },
    )
    .await
    .unwrap();
}

fn settled(fake: &Fake) -> usize {
    fake.calls()
        .iter()
        .filter(|c| c.starts_with("ack") || c.starts_with("nack"))
        .count()
}

async fn peeked(t: &Tracker<Fake>, who: &str, n: usize, f: &Filter) -> Vec<String> {
    let reply = t.peek(&agent(who), n, &[], f).await.unwrap();
    reply["next"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["item"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn a_kind_filter_takes_only_that_kind_and_spends_no_ready_record() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "r1", 2).await;
    child(&t, "t1", "r1", WorkKind::Triage, 2).await;
    seed(&t, "r2", 2).await;
    let lane = fake.ready_count("work.ready.p2");
    assert_eq!(
        next(&t, "w1", &filter(&["triage"], &[], None, &[])).await,
        Some("t1".into())
    );
    // The fixes the caller passed over were neither polled, acked nor nacked.
    assert_eq!(settled(&fake), 0);
    assert!(fake.ready_count("work.ready.p2") >= lane);
    assert_eq!(
        next(&t, "w2", &filter(&["question"], &[], None, &[])).await,
        None
    );
    assert_eq!(settled(&fake), 0);
    // The fixes are still there for an unfiltered caller.
    assert!(next(&t, "w2", &Filter::default()).await.is_some());
}

#[tokio::test]
async fn tier_repo_and_excluded_roots_narrow_the_claim() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "plain", 1).await;
    seed_in_felix(&t, "felix").await;
    seed(&t, "urgent", 0).await;
    assert_eq!(
        next(&t, "w1", &filter(&[], &[2], Some("getfelix/FELIX"), &[])).await,
        Some("felix".into())
    );
    assert_eq!(
        next(&t, "w2", &filter(&[], &[1], None, &["plain"])).await,
        None
    );
    assert_eq!(
        next(&t, "w2", &filter(&[], &[], None, &["urgent"])).await,
        Some("plain".into())
    );
    assert_eq!(
        next(&t, "w3", &Filter::default()).await,
        Some("urgent".into())
    );
}

#[tokio::test]
async fn a_filter_never_hands_out_what_the_caller_could_not_claim_without_it() {
    let fake = Fake::default();
    let t = start(&fake).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 13 })
        .await
        .unwrap();
    seed(&t, "mine", 2).await;
    seed(&t, "other", 2).await;
    t.person_record(
        key("gab"),
        WorkRecord::Reassigned {
            item: item("mine"),
            to: Some(kbl_core::work::Assignee::Agent(key("w1"))),
        },
    )
    .await
    .unwrap();
    // Another agent's item is not offered to w2 under any filter, or in a peek.
    for f in [Filter::default(), filter(&["fix"], &[2], None, &[])] {
        assert_eq!(peeked(&t, "w2", 10, &f).await, ["other"]);
    }
    assert_eq!(
        next(&t, "w2", &filter(&["fix"], &[], None, &[])).await,
        Some("other".into())
    );
    assert_eq!(
        next(&t, "w3", &filter(&["fix"], &[], None, &[])).await,
        None
    );
    // Its assignee gets it, and a filter that excludes it leaves it waiting.
    assert_eq!(
        next(&t, "w1", &filter(&["triage"], &[], None, &[])).await,
        None
    );
    assert_eq!(
        next(&t, "w1", &filter(&["fix"], &[], None, &[])).await,
        Some("mine".into())
    );
}

#[tokio::test]
async fn the_peek_lists_the_order_and_claims_nothing() {
    let fake = Fake::default();
    let t = start(&fake).await;
    for (id, tier) in [("c", 2), ("a", 2), ("b", 1)] {
        seed(&t, id, tier).await;
    }
    let before = (
        fake.log().len(),
        fake.calls().len(),
        fake.ready_count("work.ready.p2"),
    );
    let order = peeked(&t, "w1", 10, &Filter::default()).await;
    assert_eq!(order[0], "b", "the higher tier first");
    assert_eq!(order.len(), 3);
    assert_eq!(peeked(&t, "w1", 2, &Filter::default()).await, order[..2]);
    assert_eq!(
        peeked(&t, "w1", 10, &filter(&[], &[2], None, &["a"])).await,
        ["c"]
    );
    // Reading changed nothing in the log or the lanes, and the claims follow the peek.
    let after = (
        fake.log().len(),
        fake.calls().len(),
        fake.ready_count("work.ready.p2"),
    );
    assert_eq!(before.0, after.0);
    assert_eq!(before.2, after.2);
    assert_eq!(
        fake.calls()[before.1..]
            .iter()
            .filter(|c| c.starts_with("poll") || c.starts_with("ack") || c.starts_with("nack"))
            .count(),
        0
    );
    for (who, want) in ["w1", "w2", "w3"].into_iter().zip(&order) {
        assert_eq!(next(&t, who, &Filter::default()).await.as_ref(), Some(want));
    }
}

#[tokio::test]
async fn roots_take_turns_in_a_tier() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a", 2).await;
    child(&t, "a1", "a", WorkKind::Augment, 2).await;
    child(&t, "a2", "a", WorkKind::Augment, 2).await;
    seed(&t, "b", 2).await;
    seed(&t, "c", 2).await;
    let root_of = |id: &str| id.chars().next().unwrap();
    let mut roots = Vec::new();
    for who in ["w1", "w2", "w3"] {
        roots.push(root_of(&next(&t, who, &Filter::default()).await.unwrap()));
    }
    roots.sort();
    assert_eq!(
        roots,
        ['a', 'b', 'c'],
        "one item from each root before a second from any"
    );
    // The peek names the same rotation.
    let rest = peeked(&t, "w4", 10, &Filter::default()).await;
    assert_eq!(rest.len(), 2);
    assert!(rest.iter().all(|i| i.starts_with('a')));
}

#[tokio::test(start_paused = true)]
async fn an_answers_follow_up_waits_behind_other_roots_until_it_has_waited() {
    let fake = Fake::default();
    let t = start(&fake).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 14 })
        .await
        .unwrap();
    seed(&t, "asked", 2).await;
    claim(&t, "w1").await;
    let q = t
        .ask(
            &agent("w1"),
            &item("asked"),
            None,
            kbl_core::content::Content::Text("which region?".into()),
            vec![],
            None,
            Vec::new(),
        )
        .await
        .unwrap()["offset"]
        .as_u64()
        .unwrap();
    // Another root has had its turn, so by turns alone the answered root is next.
    seed(&t, "busy", 2).await;
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("busy"));
    t.release(&agent("w2"), &item("busy"), None, None, None, Vec::new())
        .await
        .unwrap();
    t.person_answer(
        key("gab"),
        &item("asked"),
        Some(q),
        kbl_core::content::Content::Text("eastus".into()),
        vec![],
        crate::tracker::Reply::default(),
    )
    .await
    .unwrap();
    let order = peeked(&t, "w3", 10, &Filter::default()).await;
    assert_eq!(
        order,
        ["busy", "asked"],
        "the follow-up queues behind the other root"
    );
    // It does not starve: after a visibility period it is next in turn.
    pass(&t, VIS + Duration::from_secs(5)).await;
    assert_eq!(
        peeked(&t, "w4", 10, &Filter::default()).await,
        ["asked", "busy"]
    );
    assert_eq!(
        next(&t, "w4", &Filter::default()).await.as_deref(),
        Some("asked")
    );
}

#[tokio::test]
async fn two_agents_never_hold_the_same_item_under_filters() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "r1", 2).await;
    seed(&t, "r2", 2).await;
    let f = filter(&["fix"], &[], None, &[]);
    let (a, b, c) = tokio::join!(next(&t, "w1", &f), next(&t, "w2", &f), next(&t, "w3", &f));
    let mut got: Vec<_> = [a, b, c].into_iter().flatten().collect();
    assert_eq!(got.len(), 2);
    got.sort();
    got.dedup();
    assert_eq!(got.len(), 2, "each item went to one agent");
}

#[tokio::test]
async fn bad_filters_are_refused_before_anything_is_tried() {
    assert!(Filter::parse(&["nonsense".into()], &[], None, &[]).is_err());
    assert!(Filter::parse(&[], &[9], None, &[]).is_err());
}
