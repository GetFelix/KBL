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
            unavailable(&t.lease(), t.lease_margin()).is_none(),
            "not writable {second} s into a hung sweep"
        );
    }
    maintain.abort();
}

#[tokio::test(start_paused = true)]
async fn the_lease_is_renewed_on_time_while_an_append_hangs() {
    let fake = Fake::default();
    fake.lease_expires();
    let t = start(&fake).await;
    let maintain = tokio::spawn({
        let t = t.clone();
        async move { t.maintain().await }
    });
    // Just before a renewal is due, an append holds the state over a write
    // Felix never answers, until the Felix timeout.
    tokio::time::sleep(Duration::from_millis(4900)).await;
    fake.hang_publishes_to(Some(WORK_EVENTS));
    let append = tokio::spawn({
        let t = t.clone();
        async move { t.person_record(key("gab"), imported("b1")).await }
    });
    let puts = fake.lease_puts();
    for tenth in 1..=150 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if tenth == 10 {
            assert!(fake.lease_puts() > puts, "no renewal while an append hangs");
        }
        assert!(fake.lease_is_held(), "the lease ran out {tenth}00 ms in");
        assert!(
            unavailable(&t.lease(), t.lease_margin()).is_none(),
            "not writable {tenth}00 ms into a hung append"
        );
    }
    assert!(append.await.unwrap().is_err());
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
    assert!(unavailable(&t.lease(), t.lease_margin()).is_none());
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
        // The grant from start must not count as a confirmation.
        tokio::time::advance(Duration::from_secs(1)).await;
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

#[tokio::test(start_paused = true)]
async fn refused_extensions_at_once_share_one_confirmation() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let who = ["w1", "w2", "w3", "w4"];
    for (n, w) in who.iter().enumerate() {
        seed(&t, &format!("a{n}"), 2).await;
        claim(&t, w).await;
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    fake.extends_are_stale(true);
    let extends: Vec<_> = who
        .iter()
        .enumerate()
        .map(|(n, w)| {
            let t = t.clone();
            let (w, i) = (agent(w), item(&format!("a{n}")));
            tokio::spawn(async move { t.extend(&w, &i).await })
        })
        .collect();
    for e in extends {
        let r = e.await.unwrap();
        assert!(matches!(r, Err(Error::Lost(_))), "{r:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_refusal_the_entry_cannot_be_read_against_does_not_fence() {
    let fake = Fake::default();
    let t = start(&fake).await;
    // A slow Felix: our renewal lands with its reply lost, then reads fail.
    fake.lose_next_lease_reply();
    fake.fail_lease_gets(true);
    t.renew_lease().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    // Refused by the old version, and the entry, ours, cannot be read.
    t.renew_lease().await;
    assert!(!t.lease().fenced, "fenced on an unread refusal");
    fake.fail_lease_gets(false);
    t.renew_lease().await;
    let lease = t.lease();
    assert!(!lease.fenced);
    assert!(unavailable(&lease, t.lease_margin()).is_none());
}

#[tokio::test(start_paused = true)]
async fn a_lease_retaken_while_confirming_a_refused_extension_is_unavailable() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    tokio::time::advance(Duration::from_secs(6)).await;
    fake.extends_are_stale(true);
    fake.hold_extends();
    let extend = tokio::spawn({
        let t = t.clone();
        async move { t.extend(&agent("w1"), &item("a1")).await }
    });
    tokio::task::yield_now().await;
    // The extension is refused once our view has run out and Felix's entry is
    // gone: the confirming renewal takes the lease again with Absent, and
    // another tracker may have served the claim in the gap.
    tokio::time::advance(Duration::from_millis(9500)).await;
    fake.expire_lease();
    fake.open_gate();
    let r = extend.await.unwrap();
    assert!(matches!(r, Err(Error::Unavailable(_))), "{r:?}");
    assert!(
        t.lease().retaken,
        "the renewal did not take the lease again"
    );
    assert!(fake.lease_is_held());
}
