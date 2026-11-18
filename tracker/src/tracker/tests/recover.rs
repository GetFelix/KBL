//! A restarted tracker takes back the deliveries Felix still holds for each
//! holder, without spending the backlog's attempts and without freeing a hold
//! it could not look for.

use super::*;

const READY: &str = "work.ready.p2";
const ALL_LANES: Option<&str> = Some("work.ready.");

fn released(fake: &Fake) -> usize {
    fake.kinds().iter().filter(|k| *k == "released").count()
}

fn tries_of(fake: &Fake, offset: u64) -> u32 {
    fake.tries(READY)
        .iter()
        .find(|(o, _)| *o == offset)
        .map_or(0, |(_, n)| *n)
}

#[tokio::test]
async fn restarts_do_not_spend_the_attempts_of_ready_records() {
    let fake = Fake::default();
    let mut t = start(&fake).await;
    // One root ready at a time, so each claim polls the lane and is a Felix handout.
    for (who, id) in [("w1", "a1"), ("w2", "a2"), ("w3", "a3")] {
        seed(&t, id, 2).await;
        claim(&t, who).await;
    }
    for id in ["b1", "b2"] {
        seed(&t, id, 2).await;
    }
    let restarts = 3;
    for _ in 0..restarts {
        drop(t);
        t = start(&fake).await;
    }
    // Each reclaim is an attempt in Felix; the backlog is never handed out.
    for held in 0..3 {
        assert_eq!(tries_of(&fake, held), 1 + restarts);
    }
    assert_eq!(tries_of(&fake, 3) + tries_of(&fake, 4), 0);
    for (who, id) in [("w1", "a1"), ("w2", "a2"), ("w3", "a3")] {
        let r = t.extend(&agent(who), &item(id)).await;
        assert!(r.is_ok(), "{who}: {r:?}");
    }
    assert_eq!(released(&fake), 0);
    assert_eq!(claimed_id(&claim(&t, "w4").await).as_deref(), Some("b1"));
}

#[tokio::test]
async fn a_holder_whose_delivery_is_gone_costs_the_backlog_one_attempt() {
    let fake = Fake::default();
    let first = start(&fake).await;
    for id in ["a1", "b1", "b2", "b3"] {
        seed(&first, id, 2).await;
    }
    claim(&first, "w1").await;
    drop(first);
    // Felix gave the delivery to another member while the tracker was down.
    fake.lapse(READY, 0);
    assert!(fake.poll_and_vanish(READY, "w9").is_some());

    start(&fake).await;
    let backlog: u32 = (1..=3).map(|o| tries_of(&fake, o)).sum();
    assert_eq!(backlog, 1);
    assert_eq!(released(&fake), 1);
}

#[tokio::test(start_paused = true)]
async fn retries_do_not_poll_a_lane_that_already_answered() {
    let fake = Fake::default();
    let first = start(&fake).await;
    seed(&first, "a1", 2).await;
    seed(&first, "b1", 2).await;
    claim(&first, "w1").await;
    drop(first);
    fake.lapse(READY, 0);
    assert!(fake.poll_and_vanish(READY, "w9").is_some());

    // Its own lane answers; another fails over and keeps failing.
    fake.fail_polls(Some("work.ready.p0"));
    let second = start(&fake).await;
    assert_eq!(tries_of(&fake, 1), 1);
    for _ in 0..ATTEMPT_BOUND {
        assert!(matches!(
            second.extend(&agent("w1"), &item("a1")).await,
            Err(Error::Felix(_))
        ));
    }
    assert_eq!(tries_of(&fake, 1), 1);
    assert_eq!(released(&fake), 0);

    // Once every lane has answered, the hold is freed.
    fake.fail_polls(None);
    assert!(matches!(
        second.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Lost(_))
    ));
    second.sweep().await;
    assert_eq!(released(&fake), 1);
    assert_eq!(tries_of(&fake, 1), 1);
}

#[tokio::test(start_paused = true)]
async fn a_poll_error_during_a_restart_keeps_the_hold_until_felix_answers() {
    let fake = Fake::default();
    let first = start(&fake).await;
    seed(&first, "a1", 2).await;
    claim(&first, "w1").await;
    drop(first);

    fake.fail_polls(ALL_LANES);
    let second = start(&fake).await;
    second.sweep().await;
    assert_eq!(released(&fake), 0);
    // The agent is asked to call again, not told its claim is lost.
    assert!(matches!(
        second.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Felix(_))
    ));
    assert!(matches!(
        second.next(&agent("w1"), Duration::ZERO).await,
        Err(Error::Felix(_))
    ));

    fake.fail_polls(None);
    pass(&second, Duration::from_secs(5)).await;
    let r = second.extend(&agent("w1"), &item("a1")).await;
    assert!(r.is_ok(), "{r:?}");
    pass(&second, VIS - Duration::from_secs(10)).await;
    second.sweep().await;
    assert_eq!(released(&fake), 0);
}

#[tokio::test(start_paused = true)]
async fn the_sweep_takes_the_delivery_back_once_felix_answers() {
    let fake = Fake::default();
    let first = start(&fake).await;
    seed(&first, "a1", 2).await;
    claim(&first, "w1").await;
    drop(first);

    fake.fail_polls(ALL_LANES);
    let second = start(&fake).await;
    fake.fail_polls(None);
    // The sweep's retry runs on a task of its own.
    second.sweep().await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    pass(&second, VIS + Duration::from_secs(5)).await;
    // Taken back by the sweep: past the deadline an agent call no longer tries.
    assert_eq!(released(&fake), 0);
    let again = claim(&second, "w1").await;
    assert_eq!(
        (again["resumed"].as_bool(), claimed_id(&again).as_deref()),
        (Some(true), Some("a1"))
    );
}

#[tokio::test(start_paused = true)]
async fn an_agent_call_takes_the_delivery_back_once_felix_answers() {
    let fake = Fake::default();
    let first = start(&fake).await;
    seed(&first, "a1", 2).await;
    claim(&first, "w1").await;
    drop(first);

    fake.fail_polls(ALL_LANES);
    let second = start(&fake).await;
    fake.fail_polls(None);
    let again = claim(&second, "w1").await;
    assert_eq!(
        (again["resumed"].as_bool(), claimed_id(&again).as_deref()),
        (Some(true), Some("a1"))
    );
    assert_eq!(fake.kinds().iter().filter(|k| *k == "claimed").count(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_hold_felix_never_answers_for_is_freed_one_visibility_after_the_restart() {
    let fake = Fake::default();
    let first = start(&fake).await;
    seed(&first, "a1", 2).await;
    claim(&first, "w1").await;
    drop(first);

    fake.fail_polls(ALL_LANES);
    let second = start(&fake).await;
    pass(&second, VIS - Duration::from_secs(5)).await;
    second.sweep().await;
    assert_eq!(released(&fake), 0);
    // By now Felix has let the delivery lapse too, so freeing it is safe.
    pass(&second, Duration::from_secs(10)).await;
    second.sweep().await;
    assert_eq!(released(&fake), 1);
    assert!(matches!(
        second.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Lost(_))
    ));
}

/// The stream's settings can change after start. The next rebuild reads them
/// again, and the tracker serves no work write until they are durable.
#[tokio::test]
async fn a_rebuild_on_a_stream_that_lost_its_durability_refuses_work() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    t.lease().refused();
    fake.weaken_acks(true);
    let err = t.extend(&agent("w1"), &item("a1")).await.unwrap_err();
    assert!(
        matches!(&err, Error::Unavailable(m) if m.contains("no longer durable")),
        "{err:?}"
    );
    assert!(t.lease().stale, "the state must stay stale while refused");
    fake.weaken_acks(false);
    t.extend(&agent("w1"), &item("a1")).await.unwrap();
}

async fn at_15(fake: &Fake) -> Tracker<Fake> {
    let t = start(fake).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 15 })
        .await
        .unwrap();
    t
}

#[tokio::test]
async fn a_direct_claim_among_several_roots_survives_a_restart() {
    let fake = Fake::default();
    let first = at_15(&fake).await;
    // Two roots ready: the claim is made from the state, not from a record.
    seed(&first, "a1", 2).await;
    seed(&first, "b1", 2).await;
    let got = claimed_id(&claim(&first, "w1").await).unwrap();
    // Another agent's poll reaches the claimed item's stale ready record and acks it.
    let other = claimed_id(&claim(&first, "w2").await).unwrap();
    assert_ne!(got, other);
    let stale = (0..2).any(|o| fake.slot(READY, o) == Slot::Acked);
    assert!(stale, "w2's poll acked nothing");
    drop(first);

    let second = start(&fake).await;
    assert_eq!(released(&fake), 0, "a restart lapsed a live direct claim");
    assert!(second.extend(&agent("w1"), &item(&got)).await.is_ok());
    assert!(second.extend(&agent("w2"), &item(&other)).await.is_ok());
    assert!(claimed_id(&claim(&second, "w3").await).is_none());
}

#[tokio::test]
async fn below_format_15_a_direct_claim_is_written_without_the_flag() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    seed(&t, "b1", 2).await;
    claim(&t, "w1").await;
    assert!(
        fake.log()
            .iter()
            .any(|l| matches!(&l.record, WorkRecord::Claimed { direct: false, .. }))
    );
    assert!(
        !fake
            .log()
            .iter()
            .any(|l| matches!(&l.record, WorkRecord::Claimed { direct: true, .. }))
    );
}
