//! Delegated workers in the tracker: tokens, caps, attribution and grants.

use super::*;
use crate::tracker::NewWorker;
use kbl_core::work::Target;

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

fn token_of(v: &Value) -> String {
    v["token"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_workers_token_works_once_its_record_lands_and_until_it_is_stopped() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    let tok = token_of(&t.start_worker(&agent("sup"), new("sup_w1")).await.unwrap());
    assert_eq!(
        t.worker_by_token(&tok),
        Some((agent("sup_w1"), agent("sup")))
    );
    // Only the hash is on the log.
    assert!(
        !fake
            .log()
            .iter()
            .any(|r| r.encode().windows(tok.len()).any(|w| w == tok.as_bytes()))
    );
    t.stop_worker(Actor::Agent, key("sup"), key("sup_w1"))
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
    let tok = token_of(&t.start_worker(&agent("sup"), new("sup_w1")).await.unwrap());
    let t = start(&fake).await;
    assert_eq!(
        t.worker_by_token(&tok),
        Some((agent("sup_w1"), agent("sup")))
    );
}

#[tokio::test]
async fn a_workers_claim_names_the_worker_and_its_supervisor() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    t.start_worker(&agent("sup"), new("sup_w1")).await.unwrap();
    seed(&t, "a1", 2).await;
    seed(&t, "a2", 2).await;
    assert_eq!(
        claimed_id(&claim(&t, "sup_w1").await).as_deref(),
        Some("a1")
    );
    let c = fake.log().last().unwrap().clone();
    assert_eq!(c.record.kind(), "claimed");
    assert_eq!(c.by.as_str(), "sup_w1");
    assert_eq!(c.sup.as_ref().map(Key::as_str), Some("sup"));
    assert_eq!(c.v, 21);
    // One hold per worker: the second call hands back the same item.
    assert_eq!(
        claimed_id(&claim(&t, "sup_w1").await).as_deref(),
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
    t.start_worker(&agent("a"), new("a_1")).await.unwrap();
    t.start_worker(&agent("a"), new("a_2")).await.unwrap();
    let over = t.start_worker(&agent("a"), new("a_3")).await.unwrap_err();
    assert!(
        matches!(over, Error::Refused { ref why } if why == "worker_cap"),
        "{over}"
    );
    t.start_worker(&agent("b"), new("b_1")).await.unwrap();
    let over = t.start_worker(&agent("b"), new("b_2")).await.unwrap_err();
    assert!(
        matches!(over, Error::Refused { ref why } if why == "worker_cap"),
        "{over}"
    );
    t.stop_worker(Actor::Person, key("gab"), key("a_1"))
        .await
        .unwrap();
    t.start_worker(&agent("b"), new("b_2")).await.unwrap();
}

#[tokio::test]
async fn a_worker_cannot_start_workers_or_stop_another_supervisors() {
    let fake = Fake::default();
    let t = at_21(&fake).await;
    t.start_worker(&agent("sup"), new("sup_w1")).await.unwrap();
    let nested = t.start_worker(&agent("sup_w1"), new("sup_w1_x")).await;
    assert!(nested.is_err());
    let other = t
        .stop_worker(Actor::Agent, key("other"), key("sup_w1"))
        .await;
    assert!(other.is_err());
    let bad = NewWorker {
        model: Some("x\nmodel".into()),
        ..new("sup_w2")
    };
    assert!(matches!(
        t.start_worker(&agent("sup"), bad).await,
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
    t.start_worker(&agent("sup"), new("sup_w1")).await.unwrap();
    let core = t.0.core.lock().await;
    assert_eq!(
        Tracker::<Fake>::grant_name(&core.state, &agent("sup_w1")),
        "sup"
    );
    assert_eq!(
        Tracker::<Fake>::grant_name(&core.state, &agent("loner")),
        "loner"
    );
}
