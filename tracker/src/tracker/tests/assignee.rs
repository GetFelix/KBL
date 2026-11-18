//! An item assigned to an agent: only that agent claims it, the others never
//! hold it or loop on it, and an assignee that went away hands it to the team.

use kbl_core::work::Assignee;

use super::*;

const AWAY: Duration = Duration::from_secs(600);

async fn at_13(fake: &Fake) -> Tracker<Fake> {
    let t = start_with(
        fake,
        Settings {
            assignee_away: AWAY,
            ..settings()
        },
    )
    .await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 13 })
        .await
        .unwrap();
    t
}

async fn assign(t: &Tracker<Fake>, id: &str, to: Option<&str>) {
    t.person_record(
        key("gab"),
        WorkRecord::Reassigned {
            item: item(id),
            to: to.map(|a| Assignee::Agent(key(a))),
        },
    )
    .await
    .unwrap();
}

fn count(fake: &Fake, call: &str) -> usize {
    fake.calls().iter().filter(|c| c.starts_with(call)).count()
}

fn claims_by(fake: &Fake, who: &str) -> usize {
    fake.log()
        .iter()
        .filter(|r| r.record.kind() == "claimed" && r.by.as_str() == who)
        .count()
}

async fn holder(t: &Tracker<Fake>) -> Option<String> {
    let core = t.0.core.lock().await;
    core.state.items[&item("a1")]
        .hold
        .as_ref()
        .map(|h| h.by.as_str().to_owned())
}

#[tokio::test]
async fn another_agent_never_holds_an_assigned_item_and_does_not_loop_on_it() {
    let fake = Fake::default();
    let t = at_13(&fake).await;
    // Published to the lane before it was assigned.
    seed(&t, "a1", 2).await;
    assign(&t, "a1", Some("w1")).await;
    assert!(claimed_id(&claim(&t, "w2").await).is_none());
    // The stale record was acked, never nacked: Felix spent no handout on it.
    assert_eq!(count(&fake, "nack"), 0);
    let (publishes, acks) = (count(&fake, "publish"), count(&fake, "ack"));
    for _ in 0..20 {
        assert!(claimed_id(&claim(&t, "w2").await).is_none());
        assert!(claimed_id(&claim(&t, "w3").await).is_none());
    }
    // Nothing was published again or handed round between the other agents.
    assert_eq!(count(&fake, "publish"), publishes);
    assert_eq!(count(&fake, "ack"), acks);
    assert_eq!(claims_by(&fake, "w2") + claims_by(&fake, "w3"), 0);
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
    assert_eq!(holder(&t).await.as_deref(), Some("w1"));
}

#[tokio::test]
async fn the_assignee_gets_its_item_ahead_of_the_lanes_and_a_release_spends_no_handout() {
    let fake = Fake::default();
    let t = at_13(&fake).await;
    seed(&t, "top", 0).await;
    seed(&t, "a1", 3).await;
    assign(&t, "a1", Some("w1")).await;
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
    t.release(&agent("w1"), &item("a1"), None, None, None, Vec::new())
        .await
        .unwrap();
    assert_eq!(count(&fake, "nack"), 0);
    // The other agent takes the lane's item, not the released one.
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("top"));
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
}

#[tokio::test]
async fn a_reassign_while_held_waits_for_the_holder_then_routes_to_the_new_assignee() {
    let fake = Fake::default();
    let t = at_13(&fake).await;
    seed(&t, "a1", 2).await;
    assign(&t, "a1", Some("w1")).await;
    claim(&t, "w1").await;
    assign(&t, "a1", Some("w2")).await;
    assert!(claimed_id(&claim(&t, "w2").await).is_none());
    assert_eq!(holder(&t).await.as_deref(), Some("w1"));
    // The holder keeps working: its extension does not touch Felix.
    assert!(t.extend(&agent("w1"), &item("a1")).await.is_ok());
    t.release(&agent("w1"), &item("a1"), None, None, None, Vec::new())
        .await
        .unwrap();
    assert!(claimed_id(&claim(&t, "w1").await).is_none());
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
}

#[tokio::test(start_paused = true)]
async fn an_assignee_that_makes_no_call_hands_the_item_to_the_team() {
    let fake = Fake::default();
    let t = at_13(&fake).await;
    seed(&t, "a1", 2).await;
    assign(&t, "a1", Some("w1")).await;
    claim(&t, "w2").await;
    pass(&t, AWAY - Duration::from_secs(10)).await;
    t.sweep().await;
    assert!(claimed_id(&claim(&t, "w2").await).is_none());
    pass(&t, Duration::from_secs(20)).await;
    t.sweep().await;
    let last = fake.log().last().cloned().unwrap();
    assert_eq!(last.record.kind(), "reassigned");
    assert_eq!(last.actor, Actor::Tracker);
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
}

#[tokio::test(start_paused = true)]
async fn an_assignee_that_keeps_calling_keeps_its_item() {
    let fake = Fake::default();
    let t = at_13(&fake).await;
    seed(&t, "other", 2).await;
    seed(&t, "a1", 2).await;
    assign(&t, "a1", Some("w1")).await;
    let first = claimed_id(&claim(&t, "w1").await).unwrap();
    assert_eq!(first, "a1");
    t.release(&agent("w1"), &item("a1"), None, None, None, Vec::new())
        .await
        .unwrap();
    // w1 takes the other item and keeps extending it, long past AWAY.
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
    t.release(&agent("w1"), &item("a1"), None, None, None, Vec::new())
        .await
        .unwrap();
    assert_eq!(claimed_id(&claim(&t, "w9").await).as_deref(), Some("other"));
    for _ in 0..12 {
        pass(&t, Duration::from_secs(60)).await;
        let _ = claim(&t, "w1").await;
        t.release(&agent("w1"), &item("a1"), None, None, None, Vec::new())
            .await
            .ok();
        t.sweep().await;
    }
    assert_eq!(
        t.0.core.lock().await.state.items[&item("a1")].assignee,
        Some(Assignee::Agent(key("w1")))
    );
}

#[tokio::test]
async fn a_revoked_assignee_hands_the_item_to_the_team_at_the_next_sweep() {
    let fake = Fake::default();
    let t = at_13(&fake).await;
    seed(&t, "a1", 2).await;
    assign(&t, "a1", Some("w1")).await;
    claim(&t, "w1").await;
    t.revoke(&agent("w1")).await.unwrap();
    t.sweep().await;
    assert_eq!(
        t.0.core.lock().await.state.items[&item("a1")].assignee,
        None
    );
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
}

#[tokio::test]
async fn a_restart_keeps_the_assignees_direct_claim_without_polling_felix() {
    let fake = Fake::default();
    let first = at_13(&fake).await;
    seed(&first, "b1", 2).await;
    seed(&first, "a1", 2).await;
    assign(&first, "a1", Some("w1")).await;
    claim(&first, "w1").await;
    drop(first);
    let nacks = count(&fake, "nack");
    let second = start(&fake).await;
    assert!(second.extend(&agent("w1"), &item("a1")).await.is_ok());
    let again = claim(&second, "w1").await;
    assert_eq!(again["resumed"].as_bool(), Some(true));
    // No other record was polled and handed back on w1's behalf.
    assert_eq!(count(&fake, "nack"), nacks);
    assert_eq!(
        claimed_id(&claim(&second, "w2").await).as_deref(),
        Some("b1")
    );
}

#[tokio::test]
async fn a_lane_claim_assigned_while_held_is_acked_not_handed_back() {
    let fake = Fake::default();
    let t = at_13(&fake).await;
    seed(&t, "a1", 2).await;
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
    assign(&t, "a1", Some("w2")).await;
    t.release(&agent("w1"), &item("a1"), None, None, None, Vec::new())
        .await
        .unwrap();
    // A nack would count a handout toward Felix's dead-letter bound for a
    // record no agent may claim from the lane.
    assert_eq!(count(&fake, "nack"), 0);
    assert_eq!(fake.slot("work.ready.p2", 0), Slot::Acked);
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
}

#[tokio::test]
async fn a_record_from_before_a_direct_claim_never_lapses_it() {
    let fake = Fake::default();
    let t = at_13(&fake).await;
    seed(&t, "a1", 2).await;
    assign(&t, "a1", Some("w1")).await;
    claim(&t, "w1").await;
    // Cleared while w1 holds it: the record published before the assignment
    // must not read as Felix handing the item out again.
    assign(&t, "a1", None).await;
    assert!(claimed_id(&claim(&t, "w2").await).is_none());
    assert_eq!(holder(&t).await.as_deref(), Some("w1"));
    assert!(t.extend(&agent("w1"), &item("a1")).await.is_ok());
    assert_eq!(count(&fake, "nack"), 0);
}

#[tokio::test]
async fn a_lapsed_hold_is_not_adopted_as_a_direct_claim() {
    let fake = Fake::default();
    let t = at_13(&fake).await;
    seed(&t, "a1", 2).await;
    assign(&t, "a1", Some("w1")).await;
    claim(&t, "w1").await;
    t.release(&agent("w1"), &item("a1"), None, None, None, Vec::new())
        .await
        .unwrap();
    // The hold ended after `recover` read it; the item is still w1's.
    let mut delivery = None;
    assert!(
        !t.adopt_direct(&agent("w1"), &item("a1"), 1, &mut delivery)
            .await
    );
    assert!(delivery.is_none());
    assert!(!t.0.core.lock().await.owner.contains_key(&item("a1")));
}

#[tokio::test(start_paused = true)]
async fn an_agent_that_calls_during_the_sweep_keeps_its_assignment() {
    let fake = Fake::default();
    let t = at_13(&fake).await;
    seed(&t, "a1", 2).await;
    seed(&t, "a2", 2).await;
    assign(&t, "a1", Some("w1")).await;
    assign(&t, "a2", Some("w1")).await;
    pass(&t, AWAY + Duration::from_secs(10)).await;
    // The first drop waits on the log; w1 calls before the second is checked.
    fake.hold_next_publish();
    let sweeping = {
        let t = t.clone();
        tokio::spawn(async move { t.drop_away_assignees().await })
    };
    tokio::task::yield_now().await;
    t.0.seen
        .lock()
        .unwrap()
        .1
        .insert("w1".to_owned(), tokio::time::Instant::now());
    fake.open_gate();
    sweeping.await.unwrap();
    let core = t.0.core.lock().await;
    let kept = ["a1", "a2"]
        .iter()
        .filter(|i| core.state.items[&item(i)].assignee.is_some())
        .count();
    assert_eq!(kept, 1);
}

/// Every ready record's handout count, across the lanes.
fn handouts(fake: &Fake) -> Vec<Vec<(u64, u32)>> {
    (0..LANES).map(|t| fake.tries(&ready_stream(t))).collect()
}

#[tokio::test]
async fn a_restart_keeps_a_direct_claim_reassigned_while_held_and_spends_no_handout() {
    let fake = Fake::default();
    let first = at_13(&fake).await;
    seed(&first, "b1", 2).await;
    seed(&first, "a1", 2).await;
    assign(&first, "a1", Some("w1")).await;
    assert_eq!(
        claimed_id(&claim(&first, "w1").await).as_deref(),
        Some("a1")
    );
    assign(&first, "a1", Some("w2")).await;
    drop(first);
    let before = handouts(&fake);
    let second = start(&fake).await;
    // The boot sweep left w1's hold alone, and nothing was polled for it.
    assert!(!fake.kinds().contains(&"released".to_owned()));
    assert_eq!(handouts(&fake), before);
    assert_eq!(count(&fake, "nack"), 0);
    assert_eq!(holder(&second).await.as_deref(), Some("w1"));
    // The new assignee waits for the holder.
    assert_eq!(
        claimed_id(&claim(&second, "w2").await).as_deref(),
        Some("b1")
    );
    second
        .complete(&agent("w2"), &item("b1"), None, Vec::new())
        .await
        .unwrap();
    assert!(second.extend(&agent("w1"), &item("a1")).await.is_ok());
    second
        .release(&agent("w1"), &item("a1"), None, None, None, Vec::new())
        .await
        .unwrap();
    assert_eq!(
        claimed_id(&claim(&second, "w2").await).as_deref(),
        Some("a1")
    );
}

#[tokio::test(start_paused = true)]
async fn a_reassigned_direct_claim_lapses_one_visibility_after_a_restart() {
    let fake = Fake::default();
    let first = at_13(&fake).await;
    seed(&first, "b1", 2).await;
    seed(&first, "a1", 2).await;
    assign(&first, "a1", Some("w1")).await;
    claim(&first, "w1").await;
    assign(&first, "a1", Some("w2")).await;
    drop(first);
    let second = start(&fake).await;
    pass(&second, VIS - Duration::from_secs(5)).await;
    second.sweep().await;
    assert_eq!(holder(&second).await.as_deref(), Some("w1"));
    // w1 stopped calling: the hold ends, and the item goes to w2.
    pass(&second, Duration::from_secs(10)).await;
    second.sweep().await;
    assert_eq!(holder(&second).await, None);
    assert_eq!(
        claimed_id(&claim(&second, "w2").await).as_deref(),
        Some("a1")
    );
}

#[tokio::test]
async fn a_lane_claim_assigned_to_its_holder_is_recovered_from_felix() {
    let fake = Fake::default();
    let first = at_13(&fake).await;
    seed(&first, "a1", 2).await;
    assert_eq!(
        claimed_id(&claim(&first, "w1").await).as_deref(),
        Some("a1")
    );
    assign(&first, "a1", Some("w1")).await;
    drop(first);
    let second = start(&fake).await;
    // Its record stands in Felix, so the restart takes back that delivery.
    let core = second.0.core.lock().await;
    let owner = core.owner.get(&item("a1")).expect("recovered");
    assert!(!owner.direct);
}
