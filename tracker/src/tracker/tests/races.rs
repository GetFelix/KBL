//! A holder hands its delivery back while Felix has already given the record
//! to someone else. The nack names an offset, so it takes that lease too.

use super::*;

const READY: &str = "work.ready.p2";

/// `who` polls the ready record now and reaches the claim step only later.
fn poll_early(t: &Tracker<Fake>, fake: &Fake, who: &str) -> (u64, GroupRecord) {
    let polled_at = t.0.settles.load(Ordering::SeqCst);
    (polled_at, fake.poll_sync(READY, who).unwrap())
}

/// The claim step of a poll made earlier. Returns whether it became a claim.
async fn claim_late(t: &Tracker<Fake>, who: &str, polled: (u64, GroupRecord)) -> bool {
    let mut ops = Vec::new();
    let mut core = t.0.core.lock().await;
    let taken = t
        .claim_from(&mut core, &agent(who), 2, polled.1, polled.0, &mut ops)
        .await
        .unwrap();
    assert!(ops.is_empty(), "the delivery is not w2's to settle");
    taken.is_some()
}

fn released(fake: &Fake) -> Vec<Option<String>> {
    fake.log()
        .into_iter()
        .filter_map(|l| match l.record {
            WorkRecord::Released { reason, .. } => Some(reason),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_release_that_races_a_claim_does_not_evict_the_next_holder() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    // w1 stalled past the visibility, so Felix hands the record to w2's poll.
    fake.lapse(READY, 0);
    let polled = poll_early(&t, &fake, "w2");
    // w1 wakes and releases before w2's poll reaches the claim step. Its nack
    // puts the record back, w2's lease included.
    t.release(&agent("w1"), &item("a1"), None, 0).await.unwrap();
    assert_eq!(fake.slot(READY, 0), Slot::Pending);
    assert!(!claim_late(&t, "w2", polled).await);
    // The record comes round to the next poll.
    assert_eq!(claimed_id(&claim(&t, "w3").await).as_deref(), Some("a1"));
    // Nobody was claimed and then lapsed without having stalled.
    assert_eq!(released(&fake), [None]);
    assert_eq!(fake.kinds().iter().filter(|k| *k == "claimed").count(), 2);
}

#[tokio::test]
async fn a_delivery_dropped_on_a_holders_next_call_does_not_evict_the_next_holder() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    // A person takes the item back while w1 hangs, and Felix hands the record on.
    t.person_record(
        key("gab"),
        WorkRecord::Released {
            item: item("a1"),
            attempt: 1,
            reason: None,
            cost_micros: 0,
        },
    )
    .await
    .unwrap();
    fake.lapse(READY, 0);
    let polled = poll_early(&t, &fake, "w2");
    // w1's next call finds its hold gone and hands the delivery back.
    assert!(matches!(
        t.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Lost(_))
    ));
    assert_eq!(fake.slot(READY, 0), Slot::Pending);
    assert!(!claim_late(&t, "w2", polled).await);
    assert_eq!(claimed_id(&claim(&t, "w3").await).as_deref(), Some("a1"));
    assert_eq!(released(&fake), [None]);
}

/// w1 releases and its nack hangs in Felix until `open_gate`.
async fn release_with_a_slow_nack(
    t: &Tracker<Fake>,
    fake: &Fake,
) -> tokio::task::JoinHandle<Result<Value, Error>> {
    fake.hold_next_nack();
    let releasing = tokio::spawn({
        let t = t.clone();
        async move { t.release(&agent("w1"), &item("a1"), None, 0).await }
    });
    for _ in 0..1000 {
        if in_flight(t).await {
            return releasing;
        }
        tokio::task::yield_now().await;
    }
    panic!("the release sent no nack");
}

async fn in_flight(t: &Tracker<Fake>) -> bool {
    t.0.core.lock().await.handed_back.get(&item("a1")) == Some(&u64::MAX)
}

async fn settled(t: &Tracker<Fake>) {
    for _ in 0..1000 {
        if !in_flight(t).await {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the hand-back never ended");
}

#[tokio::test]
async fn a_poll_made_while_the_nack_is_on_its_way_is_no_claim() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    let releasing = release_with_a_slow_nack(&t, &fake).await;
    // Felix hands the record on while the nack has not landed.
    fake.lapse(READY, 0);
    let polled = poll_early(&t, &fake, "w2");
    assert!(!claim_late(&t, "w2", polled.clone()).await);
    fake.open_gate();
    releasing.await.unwrap().unwrap();
    // The nack took that delivery, so it is still no claim once the nack ended.
    assert_eq!(fake.slot(READY, 0), Slot::Pending);
    assert!(!claim_late(&t, "w2", polled).await);
    assert_eq!(claimed_id(&claim(&t, "w3").await).as_deref(), Some("a1"));
    assert_eq!(released(&fake), [None]);
}

#[tokio::test]
async fn a_release_whose_caller_goes_away_still_frees_the_item() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    let releasing = release_with_a_slow_nack(&t, &fake).await;
    // The request is dropped while the nack is on its way.
    releasing.abort();
    assert!(releasing.await.unwrap_err().is_cancelled());
    assert!(in_flight(&t).await);
    fake.open_gate();
    settled(&t).await;
    assert_eq!(fake.slot(READY, 0), Slot::Pending);
    assert_eq!(claimed_id(&claim(&t, "w3").await).as_deref(), Some("a1"));
}

#[tokio::test]
async fn another_callers_call_does_not_end_a_hand_back_still_on_its_way() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    // w0 held an earlier delivery of the same record and has not called since.
    let record = t
        .slot(&agent("w1"))
        .lock()
        .await
        .as_ref()
        .unwrap()
        .record
        .clone();
    *t.slot(&agent("w0")).lock().await = Some(Delivery {
        id: u64::MAX,
        item: item("a1"),
        attempt: 1,
        tier: 2,
        record,
    });
    let releasing = release_with_a_slow_nack(&t, &fake).await;
    // w0 wakes while w1's nack is on its way. Its stale delivery is dropped,
    // and that must not end w1's hand-back.
    assert!(matches!(
        t.extend(&agent("w0"), &item("a1")).await,
        Err(Error::Lost(_))
    ));
    assert!(in_flight(&t).await);
    // A poll after w0's call still loses its lease to the nack.
    fake.lapse(READY, 0);
    let polled = poll_early(&t, &fake, "w2");
    assert!(!claim_late(&t, "w2", polled).await);
    fake.open_gate();
    releasing.await.unwrap().unwrap();
    assert_eq!(claimed_id(&claim(&t, "w3").await).as_deref(), Some("a1"));
    assert_eq!(released(&fake), [None]);
}

#[tokio::test]
async fn a_hand_back_mark_does_not_outlive_its_item() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    t.release(&agent("w1"), &item("a1"), None, 0).await.unwrap();
    assert!(t.0.core.lock().await.handed_back.contains_key(&item("a1")));
    // A person closes the item while it waits in Ready.
    t.person_record(
        key("gab"),
        WorkRecord::Closed {
            item: item("a1"),
            why: agents_core::work::CloseWhy::Dismissed,
        },
    )
    .await
    .unwrap();
    assert!(t.0.core.lock().await.handed_back.is_empty());
}
