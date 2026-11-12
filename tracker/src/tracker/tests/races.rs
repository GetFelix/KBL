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
