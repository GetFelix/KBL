//! A terminal call retried after its record landed (the tracker died before
//! answering, or the answer was lost) is told what it already wrote, but only
//! for the same agent, the same attempt and a record that agent wrote.

use kbl_core::work::Phase;

use super::*;
use crate::tracker::Decide;

/// The attempt `who` claimed, and the `claimed` record's offset.
async fn claim_attempt(t: &Tracker<Fake>, fake: &Fake, who: &str) -> u32 {
    let reply = claim(t, who).await;
    assert_eq!(
        reply["offset"],
        last(fake),
        "the reply names the claimed record: {reply}"
    );
    reply["attempt"].as_u64().unwrap() as u32
}

/// Refused as it is today: the claim is lost or not held.
fn not_replayed(r: Result<Value, Error>) -> bool {
    matches!(r, Err(Error::Lost(_) | Error::NotHeld(_)))
}

fn last(fake: &Fake) -> u64 {
    fake.log().len() as u64 - 1
}

/// Ends `who`'s claim on a1 the way `how` says, sending `attempt`.
async fn end(
    t: &Tracker<Fake>,
    how: &str,
    who: &str,
    attempt: Option<u32>,
) -> Result<Value, Error> {
    let (w, a1) = (agent(who), item("a1"));
    match how {
        "done" => t.complete(&w, &a1, attempt, Vec::new()).await,
        "released" => t.release(&w, &a1, attempt, None, Vec::new()).await,
        "gave_up" => {
            t.give_up(&w, &a1, attempt, "stuck".into(), Vec::new())
                .await
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn each_terminal_call_retried_after_it_landed_is_replayed() {
    for how in ["done", "released", "gave_up"] {
        let fake = Fake::default();
        let t = start(&fake).await;
        seed(&t, "a1", 2).await;
        let attempt = claim_attempt(&t, &fake, "w1").await;
        let first = end(&t, how, "w1", Some(attempt)).await.unwrap();
        let at = last(&fake);
        assert_eq!(first["offset"], at, "{how}");
        assert!(first.get("replayed").is_none(), "{how}");
        let kinds = fake.kinds();
        let again = end(&t, how, "w1", Some(attempt)).await.unwrap();
        assert_eq!(again["replayed"], true, "{how}");
        assert_eq!(again["offset"], at, "{how}");
        assert_eq!(again["status"], first["status"], "{how}");
        // A tracker that starts on the log after a crash answers the same.
        let t = start(&fake).await;
        let again = end(&t, how, "w1", Some(attempt)).await.unwrap();
        assert_eq!(
            (&again["replayed"], &again["offset"]),
            (&json!(true), &json!(at)),
            "{how}"
        );
        assert_eq!(fake.kinds(), kinds, "nothing more is written: {how}");
    }
}

#[tokio::test]
async fn a_call_whose_write_landed_but_whose_reply_was_lost_is_replayed() {
    for how in ["done", "released", "gave_up"] {
        let fake = Fake::default();
        let t = start(&fake).await;
        seed(&t, "a1", 2).await;
        let attempt = claim_attempt(&t, &fake, "w1").await;
        fake.lose_next_publish_reply(WORK_EVENTS);
        assert!(end(&t, how, "w1", Some(attempt)).await.is_err(), "{how}");
        let at = last(&fake);
        assert_eq!(fake.kinds().last().map(String::as_str), Some(how));
        // The tracker still has the delivery; the retry settles it and answers.
        let again = end(&t, how, "w1", Some(attempt)).await.unwrap();
        assert_eq!(
            (&again["replayed"], &again["offset"]),
            (&json!(true), &json!(at)),
            "{how}"
        );
        assert_eq!(fake.log().len() as u64, at + 1, "{how}");
    }
}

#[tokio::test]
async fn without_an_attempt_a_retry_is_not_replayed() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    end(&t, "done", "w1", None).await.unwrap();
    assert!(not_replayed(end(&t, "done", "w1", None).await));
}

#[tokio::test]
async fn another_agent_or_attempt_or_kind_is_not_replayed() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    let attempt = claim_attempt(&t, &fake, "w1").await;
    end(&t, "done", "w1", Some(attempt)).await.unwrap();
    assert!(not_replayed(end(&t, "done", "w2", Some(attempt)).await));
    assert!(not_replayed(end(&t, "done", "w1", Some(attempt + 1)).await));
    assert!(not_replayed(end(&t, "gave_up", "w1", Some(attempt)).await));
}

#[tokio::test(start_paused = true)]
async fn a_claim_the_tracker_released_as_lapsed_is_still_lost() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    let attempt = claim_attempt(&t, &fake, "w1").await;
    pass(&t, VIS + Duration::from_secs(1)).await;
    t.sweep().await;
    assert_eq!(fake.log().last().unwrap().actor, Actor::Tracker);
    assert_eq!(fake.kinds().last().map(String::as_str), Some("released"));
    assert!(matches!(
        end(&t, "released", "w1", Some(attempt)).await,
        Err(Error::Lost(_))
    ));
}

#[tokio::test]
async fn a_release_retried_after_another_agent_took_the_same_attempt_is_replayed() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    let first = claim_attempt(&t, &fake, "w1").await;
    end(&t, "released", "w1", Some(first)).await.unwrap();
    let at = last(&fake);
    // A retry after a release claims the same attempt number again.
    assert_eq!(claim_attempt(&t, &fake, "w2").await, first);
    let again = end(&t, "released", "w1", Some(first)).await.unwrap();
    assert_eq!(
        (&again["replayed"], &again["offset"]),
        (&json!(true), &json!(at))
    );
}

#[tokio::test(start_paused = true)]
async fn only_the_agents_latest_claim_of_the_attempt_is_replayed() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    let attempt = claim_attempt(&t, &fake, "w1").await;
    end(&t, "released", "w1", Some(attempt)).await.unwrap();
    // w1 claims the same attempt again, and the tracker frees it as lapsed.
    fake.lapse("work.ready.p2", 0);
    assert_eq!(claim_attempt(&t, &fake, "w1").await, attempt);
    pass(&t, VIS + Duration::from_secs(1)).await;
    t.sweep().await;
    assert_eq!(fake.log().last().unwrap().actor, Actor::Tracker);
    // The earlier release by w1 does not answer for this claim.
    assert!(matches!(
        end(&t, "released", "w1", Some(attempt)).await,
        Err(Error::Lost(_))
    ));
}

#[tokio::test]
async fn a_retry_for_an_earlier_attempt_does_not_end_the_agents_next_one() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    let first = claim_attempt(&t, &fake, "w1").await;
    let d = Decide {
        phase: Phase::Verify,
        so_far: "fix in".into(),
        question: String::new(),
        options: vec![],
        chosen: String::new(),
        why: "the test passes".into(),
        commit: "abc123".into(),
        evidence: vec![],
        revert_check: None,
    };
    let at = t.decide(&agent("w1"), &item("a1"), d).await.unwrap()["offset"]
        .as_u64()
        .unwrap();
    end(&t, "done", "w1", Some(first)).await.unwrap();
    // A person asks for a redo, so the next claim is a new attempt.
    let redo = WorkRecord::ForkedFrom {
        item: item("a1"),
        from_attempt: first,
        at,
        note: None,
    };
    t.person_record(key("gab"), redo).await.unwrap();
    let second = claim_attempt(&t, &fake, "w1").await;
    assert_ne!(first, second);
    let kinds = fake.kinds();
    let again = end(&t, "done", "w1", Some(first)).await.unwrap();
    assert_eq!(again["replayed"], true);
    assert_eq!(fake.kinds(), kinds, "the second attempt still stands");
    assert!(t.extend(&agent("w1"), &item("a1")).await.is_ok());
}
