//! A blocked item is handed out by no claim path, spends no ready record
//! while it waits, and stays blocked across a restart.

use kbl_core::work::Assignee;

use super::*;

async fn at_16(fake: &Fake) -> Tracker<Fake> {
    let t = start(fake).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 16 })
        .await
        .unwrap();
    t
}

async fn block(t: &Tracker<Fake>, id: &str, until_ms: Option<u64>) {
    t.person_record(
        key("gab"),
        WorkRecord::BlockSet {
            item: item(id),
            until_ms,
            resource: Some("azure-vms".into()),
            reason: "Azure credit is at zero".into(),
        },
    )
    .await
    .unwrap();
}

async fn lift(t: &Tracker<Fake>, id: &str) {
    t.person_record(key("gab"), WorkRecord::BlockCleared { item: item(id) })
        .await
        .unwrap();
}

fn count(fake: &Fake, call: &str) -> usize {
    fake.calls().iter().filter(|c| c.starts_with(call)).count()
}

fn claims(fake: &Fake, id: &str) -> usize {
    fake.log()
        .iter()
        .filter(|r| {
            r.record.kind() == "claimed" && r.record.item().is_some_and(|i| i.as_str() == id)
        })
        .count()
}

fn all_filter() -> Filter {
    Filter::parse(&["fix".to_string()], &[], None, &[]).unwrap()
}

#[tokio::test]
async fn a_blocked_item_on_the_lane_is_skipped_and_comes_back_when_lifted() {
    let fake = Fake::default();
    let t = at_16(&fake).await;
    seed(&t, "a1", 2).await;
    block(&t, "a1", None).await;
    for who in ["w1", "w2", "w1"] {
        assert!(claimed_id(&claim(&t, who).await).is_none());
    }
    assert_eq!(claims(&fake, "a1"), 0);
    // Acked, never nacked: Felix spent no handout on it, and it is not published again.
    assert_eq!(count(&fake, "nack"), 0);
    let publishes = count(&fake, "publish");
    assert!(claimed_id(&claim(&t, "w2").await).is_none());
    assert_eq!(count(&fake, "publish"), publishes);
    lift(&t, "a1").await;
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
}

#[tokio::test]
async fn a_blocked_root_is_passed_over_by_a_direct_claim_and_spends_nothing() {
    let fake = Fake::default();
    let t = at_16(&fake).await;
    seed(&t, "a1", 2).await;
    seed(&t, "b1", 2).await;
    seed(&t, "c1", 2).await;
    block(&t, "a1", None).await;
    // Several roots ready: the claim is made from the state, so no record is polled
    // and the blocked item's record is left alone.
    let polls = count(&fake, "poll");
    let first = claimed_id(&claim(&t, "w1").await).unwrap();
    assert_eq!(count(&fake, "poll"), polls);
    assert_eq!(fake.slot("work.ready.p2", 0), Slot::Pending);
    assert_eq!(
        fake.tries("work.ready.p2")
            .iter()
            .map(|(_, n)| n)
            .sum::<u32>(),
        0
    );
    let second = claimed_id(&claim(&t, "w2").await).unwrap();
    assert_ne!(first, second);
    assert!(![first, second].contains(&"a1".to_owned()));
    // With one root left the lane is polled: the blocked record is acked once and
    // not published again until the block ends.
    assert!(claimed_id(&claim(&t, "w3").await).is_none());
    let (publishes, polls) = (count(&fake, "publish"), count(&fake, "poll"));
    assert!(claimed_id(&claim(&t, "w3").await).is_none());
    assert_eq!(count(&fake, "publish"), publishes);
    assert!(count(&fake, "poll") <= polls + 1);
    // The filtered path too.
    let f = all_filter();
    let reply = t
        .next_filtered(&agent("w4"), Duration::ZERO, &[], &f)
        .await
        .unwrap();
    assert!(claimed_id(&reply).is_none());
    assert_eq!(claims(&fake, "a1"), 0);
    assert_eq!(count(&fake, "nack"), 0);
}

#[tokio::test]
async fn an_assigned_item_is_not_handed_to_its_agent_while_blocked() {
    let fake = Fake::default();
    let t = at_16(&fake).await;
    seed(&t, "a1", 2).await;
    seed(&t, "b1", 2).await;
    t.person_record(
        key("gab"),
        WorkRecord::Reassigned {
            item: item("a1"),
            to: Some(Assignee::Agent(key("w1"))),
        },
    )
    .await
    .unwrap();
    block(&t, "a1", None).await;
    // Its blocked assignment does not starve the rest.
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("b1"));
    assert_eq!(claims(&fake, "a1"), 0);
    lift(&t, "a1").await;
    t.extend(&agent("w1"), &item("b1")).await.unwrap();
    // w1 holds b1; a second agent is not handed the assigned item.
    assert!(claimed_id(&claim(&t, "w2").await).is_none());
}

#[tokio::test]
async fn a_peek_leaves_out_a_blocked_item() {
    let fake = Fake::default();
    let t = at_16(&fake).await;
    seed(&t, "a1", 2).await;
    seed(&t, "b1", 2).await;
    block(&t, "a1", None).await;
    let f = Filter::default();
    let reply = t.peek(&agent("w1"), 10, &[], &f).await.unwrap();
    let ids: Vec<&str> = reply["next"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["item"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["b1"]);
}

#[tokio::test]
async fn a_blocked_item_is_never_handed_out_across_a_restart() {
    let fake = Fake::default();
    let t = at_16(&fake).await;
    seed(&t, "a1", 2).await;
    seed(&t, "b1", 2).await;
    block(&t, "a1", None).await;
    drop(t);
    let t = start(&fake).await;
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("b1"));
    assert!(claimed_id(&claim(&t, "w2").await).is_none());
    drop(t);
    let t = start(&fake).await;
    assert!(claimed_id(&claim(&t, "w2").await).is_none());
    assert_eq!(claims(&fake, "a1"), 0);
    lift(&t, "a1").await;
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
}

#[tokio::test]
async fn a_block_with_a_time_ends_by_itself_even_after_a_restart() {
    let fake = Fake::default();
    let t = at_16(&fake).await;
    seed(&t, "a1", 2).await;
    block(&t, "a1", Some(now_ms() + 600)).await;
    assert!(claimed_id(&claim(&t, "w1").await).is_none());
    drop(t);
    let t = start(&fake).await;
    assert!(claimed_id(&claim(&t, "w1").await).is_none());
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
}

#[tokio::test]
async fn an_item_that_is_blocked_while_held_keeps_its_hold() {
    let fake = Fake::default();
    let t = at_16(&fake).await;
    seed(&t, "a1", 2).await;
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
    block(&t, "a1", None).await;
    t.extend(&agent("w1"), &item("a1")).await.unwrap();
    assert!(!fake.kinds().iter().any(|k| k == "released"));
    // Released, it waits for the block to end instead of going back out.
    t.release(&agent("w1"), &item("a1"), None, None, None, Vec::new())
        .await
        .unwrap();
    assert!(claimed_id(&claim(&t, "w2").await).is_none());
}

#[tokio::test]
async fn a_block_is_refused_below_format_16() {
    let fake = Fake::default();
    let t = at_15(&fake).await;
    seed(&t, "a1", 2).await;
    let err = t
        .person_record(
            key("gab"),
            WorkRecord::BlockSet {
                item: item("a1"),
                until_ms: None,
                resource: Some("azure-vms".into()),
                reason: "no credit".into(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Refused { ref why } if why == "format"),
        "{err:?}"
    );
    assert!(!fake.kinds().iter().any(|k| k == "block_set"));
}
