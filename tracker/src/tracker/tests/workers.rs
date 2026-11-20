//! Delegated workers in the tracker: tokens, caps, attribution and grants.

use super::*;
use crate::tracker::NewWorker;
use crate::tracker::workers::LiveWorker;
use kbl_core::org::TokenHash;
use kbl_core::work::{SUPERVISOR_GONE, Target};

fn new(worker: &str) -> NewWorker {
    NewWorker {
        worker: agent(worker),
        vendor: Some("fake".into()),
        profile: Some("fake".into()),
        model: Some("small".into()),
    }
}

async fn at_21(fake: &Fake) -> Tracker<Fake> {
    let t = start(fake).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 21 })
        .await
        .unwrap();
    t
}

async fn at_24(fake: &Fake) -> Tracker<Fake> {
    let t = start(fake).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 24 })
        .await
        .unwrap();
    t
}

/// Starts `worker` for `sup`, bound to the token `sup` has in these tests.
async fn start_for(t: &Tracker<Fake>, sup: &str, worker: &str) -> Result<Value, Error> {
    t.start_worker(
        &agent(sup),
        &TokenHash::of(&format!("tok-{sup}")),
        new(worker),
    )
    .await
}

fn token_of(v: &Value) -> String {
    v["token"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_workers_token_works_once_its_record_lands_and_until_it_is_stopped() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    let tok = token_of(&start_for(&t, "sup", "sup-w1").await.unwrap());
    assert_eq!(
        t.worker_by_token(&tok),
        Some(LiveWorker {
            worker: agent("sup-w1"),
            supervisor: agent("sup"),
            supervisor_token: TokenHash::of("tok-sup"),
        })
    );
    // Only the hash is on the log.
    assert!(
        !fake
            .log()
            .iter()
            .any(|r| r.encode().windows(tok.len()).any(|w| w == tok.as_bytes()))
    );
    t.stop_worker(Actor::Agent, key("sup"), key("sup-w1"))
        .await
        .unwrap();
    assert_eq!(t.worker_by_token(&tok), None);
    // A restart rebuilds the map from the log: still stopped.
    let t = start(&fake).await;
    assert_eq!(t.worker_by_token(&tok), None);
}

#[tokio::test]
async fn a_restart_keeps_a_live_workers_token() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    let tok = token_of(&start_for(&t, "sup", "sup-w1").await.unwrap());
    let t = start(&fake).await;
    assert_eq!(
        t.worker_by_token(&tok),
        Some(LiveWorker {
            worker: agent("sup-w1"),
            supervisor: agent("sup"),
            supervisor_token: TokenHash::of("tok-sup"),
        })
    );
}

#[tokio::test]
async fn a_workers_claim_names_the_worker_and_its_supervisor() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    start_for(&t, "sup", "sup-w1").await.unwrap();
    seed(&t, "a1", 2).await;
    seed(&t, "a2", 2).await;
    assert_eq!(
        claimed_id(&claim(&t, "sup-w1").await).as_deref(),
        Some("a1")
    );
    let c = fake.log().last().unwrap().clone();
    assert_eq!(c.record.kind(), "claimed");
    assert_eq!(c.by.as_str(), "sup-w1");
    assert_eq!(c.sup.as_ref().map(Key::as_str), Some("sup"));
    assert_eq!(c.v, 21);
    // One hold per worker: the second call hands back the same item.
    assert_eq!(
        claimed_id(&claim(&t, "sup-w1").await).as_deref(),
        Some("a1")
    );
    let listed = t.workers(Some(&key("sup"))).await;
    assert_eq!(listed["workers"][0]["holds"]["item"], "a1");
    assert_eq!(listed["workers"][0]["model"], "small");
}

#[tokio::test]
async fn workers_are_capped_per_supervisor_and_per_team() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    // The test settings allow two per supervisor and three per team.
    start_for(&t, "a", "a-1").await.unwrap();
    start_for(&t, "a", "a-2").await.unwrap();
    let over = start_for(&t, "a", "a-3").await.unwrap_err();
    assert!(
        matches!(over, Error::Refused { ref why } if why == "worker_cap"),
        "{over}"
    );
    start_for(&t, "b", "b-1").await.unwrap();
    let over = start_for(&t, "b", "b-2").await.unwrap_err();
    assert!(
        matches!(over, Error::Refused { ref why } if why == "worker_cap"),
        "{over}"
    );
    t.stop_worker(Actor::Person, key("gab"), key("a-1"))
        .await
        .unwrap();
    start_for(&t, "b", "b-2").await.unwrap();
}

#[tokio::test]
async fn a_worker_cannot_start_workers_or_stop_another_supervisors() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    start_for(&t, "sup", "sup-w1").await.unwrap();
    let nested = start_for(&t, "sup-w1", "sup-x").await;
    assert!(nested.is_err());
    let other = t
        .stop_worker(Actor::Agent, key("other"), key("sup-w1"))
        .await;
    assert!(other.is_err());
    let bad = NewWorker {
        model: Some("x\nmodel".into()),
        ..new("sup-w2")
    };
    assert!(matches!(
        t.start_worker(&agent("sup"), &TokenHash::of("tok-sup"), bad)
            .await,
        Err(Error::Invalid(_))
    ));
}

#[tokio::test]
async fn a_worker_reaches_only_its_supervisors_grants() {
    let fake = Fake::default();
    let mut s = settings();
    let target = Target {
        source: "github".into(),
        repo: "GetFelix/other".into(),
    };
    s.grants.insert("sup".into(), vec![target.clone()]);
    let t = start_with(&fake, s).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 21 })
        .await
        .unwrap();
    start_for(&t, "sup", "sup-w1").await.unwrap();
    let core = t.0.core.lock().await;
    assert_eq!(
        Tracker::<Fake>::grant_name(&core.state, &agent("sup-w1")),
        "sup"
    );
    assert_eq!(
        Tracker::<Fake>::grant_name(&core.state, &agent("loner")),
        "loner"
    );
}

#[tokio::test]
async fn labels_with_hidden_format_characters_are_refused() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    for hidden in ["a\u{200B}b", "a\u{202E}b", "a\u{2067}b"] {
        let bad = NewWorker {
            model: Some(hidden.into()),
            ..new("sup-w1")
        };
        assert!(matches!(
            t.start_worker(&agent("sup"), &TokenHash::of("tok-sup"), bad)
                .await,
            Err(Error::Invalid(_))
        ));
    }
}

fn tries_on(state: &kbl_core::work_fold::State, item: &str) -> u32 {
    state.items[&ItemId::new(item).unwrap()].paths[0].tries
}

#[tokio::test]
async fn stopping_a_gone_supervisors_workers_frees_their_holds_at_no_cost() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    let tok = token_of(&start_for(&t, "sup", "sup-w1").await.unwrap());
    start_for(&t, "other", "other-w1").await.unwrap();
    seed(&t, "a1", 2).await;
    seed(&t, "a2", 2).await;
    assert_eq!(
        claimed_id(&claim(&t, "sup-w1").await).as_deref(),
        Some("a1")
    );
    assert_eq!(
        claimed_id(&claim(&t, "other-w1").await).as_deref(),
        Some("a2")
    );
    let released = t
        .stop_workers(|w| w.supervisor == key("sup"))
        .await
        .unwrap();
    assert_eq!(released, vec![item("a1")]);
    assert_eq!(t.worker_by_token(&tok), None);
    let log = fake.log();
    let kinds: Vec<_> = log.iter().rev().take(2).map(|r| r.record.kind()).collect();
    assert_eq!(kinds, ["released", "worker_stopped"]);
    let last = log.last().unwrap();
    assert!(matches!(
        &last.record,
        WorkRecord::Released { reason: Some(r), .. } if r == SUPERVISOR_GONE
    ));
    let core = t.0.core.lock().await;
    assert!(
        core.state.rejections.is_empty(),
        "{:?}",
        core.state.rejections
    );
    assert_eq!(tries_on(&core.state, "a1"), 0);
    // The other supervisor's worker keeps its hold.
    assert_eq!(
        core.state.held_by("other-w1").map(|h| h.0),
        Some(item("a2"))
    );
}

#[tokio::test]
async fn revoking_a_supervisor_stops_its_workers() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    let tok = token_of(&start_for(&t, "sup", "sup-w1").await.unwrap());
    seed(&t, "a1", 2).await;
    claim(&t, "sup-w1").await;
    let released = t.revoke(&agent("sup")).await.unwrap();
    assert_eq!(released, vec![item("a1")]);
    assert_eq!(t.worker_by_token(&tok), None);
    let core = t.0.core.lock().await;
    assert!(core.state.workers[&key("sup-w1")].stopped_ms.is_some());
    assert_eq!(tries_on(&core.state, "a1"), 0);
}

#[tokio::test]
async fn stopping_a_worker_gives_back_its_hold_at_once_at_no_cost() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    start_for(&t, "sup", "sup-w1").await.unwrap();
    seed(&t, "a1", 2).await;
    claim(&t, "sup-w1").await;
    let stop = t
        .stop_worker(Actor::Agent, key("sup"), key("sup-w1"))
        .await
        .unwrap();
    assert_eq!(stop["released"], json!(["a1"]));
    {
        let core = t.0.core.lock().await;
        assert!(core.state.held_by("sup-w1").is_none());
        assert_eq!(tries_on(&core.state, "a1"), 0);
        assert!(core.state.rejections.is_empty());
    }
    // Another agent takes it without waiting for the claim to run out.
    fake.lapse_all();
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
}

#[tokio::test]
async fn a_stopped_worker_still_holding_is_released_on_the_next_sweep() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    start_for(&t, "sup", "sup-w1").await.unwrap();
    seed(&t, "a1", 2).await;
    claim(&t, "sup-w1").await;
    // The stop lands, the release after it does not: the tracker dies between.
    fake.fail_publishes_to_after(WORK_EVENTS, 1);
    t.stop_worker(Actor::Agent, key("sup"), key("sup-w1"))
        .await
        .unwrap();
    fake.lift_publish_budget();
    assert_eq!(fake.log().last().unwrap().record.kind(), "worker_stopped");
    let t = start(&fake).await;
    t.sweep().await;
    let core = t.0.core.lock().await;
    assert!(core.state.held_by("sup-w1").is_none());
    assert_eq!(tries_on(&core.state, "a1"), 0);
    assert!(core.state.rejections.is_empty());
}

#[tokio::test]
async fn a_worker_that_finishes_ends_and_gives_back_what_it_holds() {
    let fake = Fake::default();
    let t = at_24(&fake).await;
    let tok = token_of(&start_for(&t, "sup", "sup-w1").await.unwrap());
    seed(&t, "a1", 2).await;
    claim(&t, "sup-w1").await;
    let done = t.finish_worker(&agent("sup-w1")).await.unwrap();
    assert_eq!(done["released"], json!(["a1"]));
    assert_eq!(t.worker_by_token(&tok), None);
    let listed = t.workers(Some(&key("sup"))).await;
    assert_eq!(listed["workers"][0]["finished"], true);
    assert!(listed["workers"][0]["stopped_ms"].is_number());
    assert!(listed["workers"][0]["holds"].is_null());
    {
        let core = t.0.core.lock().await;
        assert_eq!(
            tries_on(&core.state, "a1"),
            1,
            "finishing abandons the item"
        );
        assert!(core.state.rejections.is_empty());
    }
    // Once is all: the token is dead, so a second finish is refused.
    assert!(t.finish_worker(&agent("sup-w1")).await.is_err());
    // A restart keeps it finished.
    let t = start(&fake).await;
    assert_eq!(t.worker_by_token(&tok), None);
    assert_eq!(t.workers(None).await["workers"][0]["finished"], true);
}

#[tokio::test]
async fn finishing_needs_the_log_at_format_24() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    start_for(&t, "sup", "sup-w1").await.unwrap();
    let err = t.finish_worker(&agent("sup-w1")).await.unwrap_err();
    assert!(
        matches!(err, Error::Format(_)),
        "{err}"
    );
    // A plain agent has no worker to finish.
    let t = at_24(&Fake::default()).await;
    assert!(t.finish_worker(&agent("sup")).await.is_err());
}

#[tokio::test]
async fn a_worker_cannot_get_its_try_back_by_finishing_over_and_over() {
    let fake = Fake::default();
    let t = at_24(&fake).await;
    seed(&t, "a1", 2).await;
    let mut spent = 0;
    for n in 1..=4 {
        let name = format!("sup-w{n}");
        start_for(&t, "sup", &name).await.unwrap();
        if claimed_id(&claim(&t, &name).await).is_none() {
            break;
        }
        t.finish_worker(&agent(&name)).await.unwrap();
        fake.lapse_all();
        spent = tries_on(&t.0.core.lock().await.state, "a1");
        // The cap is two live workers; ended ones free their slot.
    }
    assert!(spent >= 2, "each finish should spend a try, got {spent}");
    // Out of tries, the item is no longer handed out.
    start_for(&t, "sup", "sup-w9").await.unwrap();
    assert_eq!(claimed_id(&claim(&t, "sup-w9").await), None);
}

#[tokio::test]
async fn a_failed_release_after_a_finish_is_reported_and_the_sweep_finishes_it() {
    let fake = Fake::default();
    let t = at_24(&fake).await;
    start_for(&t, "sup", "sup-w1").await.unwrap();
    seed(&t, "a1", 2).await;
    claim(&t, "sup-w1").await;
    fake.fail_publishes_to_after(WORK_EVENTS, 1);
    let done = t.finish_worker(&agent("sup-w1")).await.unwrap();
    assert!(done["release_error"].is_string(), "{done}");
    assert_eq!(done["released"], json!([]));
    fake.lift_publish_budget();
    t.sweep().await;
    let core = t.0.core.lock().await;
    assert!(core.state.held_by("sup-w1").is_none());
    assert_eq!(tries_on(&core.state, "a1"), 1);
}
