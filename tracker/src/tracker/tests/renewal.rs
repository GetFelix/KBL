//! The writer lease keeps its renewals on time whatever else is slow, and a
//! refused extension is answered `lost` only by the lease's holder.

use super::*;

#[tokio::test(start_paused = true)]
async fn the_lease_is_renewed_on_time_while_a_sweep_call_hangs() {
    let fake = Fake::default();
    fake.lease_expires();
    let t = start(&fake).await;
    for id in ["a1", "a2", "a3"] {
        seed(&t, id, 2).await;
    }
    // Ready lanes stop answering, and the sweep has records to publish there.
    fake.hang_publishes_to(Some("work.ready."));
    t.0.core.lock().await.live.clear();
    let maintain = tokio::spawn({
        let t = t.clone();
        async move { t.maintain().await }
    });
    for second in 1..=40 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(
            fake.lease_is_held(),
            "the lease ran out {second} s into a hung sweep"
        );
        assert!(
            unavailable(&*t.0.core.lock().await, t.lease_margin()).is_none(),
            "not writable {second} s into a hung sweep"
        );
    }
    maintain.abort();
}

#[tokio::test(start_paused = true)]
async fn a_hung_lease_put_gives_up_before_the_next_renewal_is_due() {
    let fake = Fake::default();
    let t = start(&fake).await;
    fake.hold_next_lease_put();
    let every = settings().lease_ttl / 3;
    assert!(
        tokio::time::timeout(every, t.renew_lease()).await.is_ok(),
        "a hung lease put held the renewal past the next one's turn"
    );
    t.renew_lease().await;
    assert!(unavailable(&*t.0.core.lock().await, t.lease_margin()).is_none());
}

#[tokio::test(start_paused = true)]
async fn a_refused_extension_is_not_lost_while_the_lease_is_in_doubt() {
    // Felix ran our lease out early and another tracker took it, or the lease
    // cannot be read at all. Either way the claim may be served elsewhere.
    for taken in [true, false] {
        let fake = Fake::default();
        let t = start(&fake).await;
        seed(&t, "a1", 2).await;
        claim(&t, "w1").await;
        fake.extends_are_stale(true);
        if taken {
            fake.expire_lease();
            Tracker::start(fake.reconnect(), settings()).await.unwrap();
        } else {
            fake.fail_lease(true);
        }
        let r = t.extend(&agent("w1"), &item("a1")).await;
        assert!(
            matches!(r, Err(Error::Unavailable(_))),
            "taken {taken}: {r:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_refused_extension_is_lost_once_the_lease_is_confirmed() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    let puts = fake.lease_puts();
    fake.extends_are_stale(true);
    let r = t.extend(&agent("w1"), &item("a1")).await;
    assert!(matches!(r, Err(Error::Lost(_))), "{r:?}");
    assert!(
        fake.lease_puts() > puts,
        "answered lost without asking Felix"
    );
}
