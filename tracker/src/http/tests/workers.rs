//! The worker routes: a supervisor's agent token starts, lists and stops
//! workers, and a worker's token acts as that worker until it is stopped.

use super::*;

async fn app_21() -> Router {
    let felix = Quiet::default();
    felix.keeps.store(true, Ordering::SeqCst);
    let tokens =
        r#"{"agents":{"tok-w1":"w1","tok-dup":"w1_dup"},"people":{"tok-gab":"gab"},"ingest":{}}"#;
    let one = team("default", felix, tokens, 64).await;
    let app = router(Arc::new(Teams::new(vec![one]).unwrap()));
    raise_to(&app, 21).await;
    app
}

#[tokio::test]
async fn a_supervisor_starts_a_worker_whose_token_acts_until_stopped() {
    let app = app_21().await;
    let start = json!({ "worker": "w1-a", "vendor": "fake", "profile": "fake", "model": "small" });
    let (status, reply) = call(&app, "/v1/work/workers", Some("tok-w1"), start).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let tok = reply["token"].as_str().unwrap().to_owned();
    let next = json!({ "wait_seconds": 0 });
    let (status, _) = call(&app, "/v1/work/next", Some(&tok), next.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (_, listed) = get_json(&app, "/v1/work/workers", Some("tok-w1")).await;
    assert_eq!(listed["workers"][0]["worker"], "w1-a");
    let (_, all) = get_json(&app, "/v1/work/workers", Some("tok-gab")).await;
    assert_eq!(all["workers"].as_array().unwrap().len(), 1);
    // A worker cannot start workers of its own.
    let nested = json!({ "worker": "w1-a-x" });
    let (status, _) = call(&app, "/v1/work/workers", Some(&tok), nested).await;
    assert_ne!(status, StatusCode::OK);
    let stop = json!({ "worker": "w1-a" });
    let (status, _) = call(&app, "/v1/work/workers/stop", Some("tok-w1"), stop).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "/v1/work/next", Some(&tok), next).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_hook_worker_trades_its_code_for_its_token_once() {
    let app = app_21().await;
    let start = json!({ "worker": "w1-h", "exchange": true });
    let (status, reply) = call(&app, "/v1/work/workers", Some("tok-w1"), start).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(
        reply.get("token").is_none(),
        "the token stays in the tracker: {reply}"
    );
    let code = reply["code"].as_str().unwrap().to_owned();
    let trade = json!({ "code": code });
    let (status, got) = call(&app, "/v1/work/workers/exchange", None, trade.clone()).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["worker"], "w1-h");
    let tok = got["token"].as_str().unwrap().to_owned();
    let (status, _) = call(
        &app,
        "/v1/work/next",
        Some(&tok),
        json!({ "wait_seconds": 0 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "/v1/work/workers/exchange", None, trade).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a code works once");
}

#[tokio::test]
async fn a_worker_name_belongs_to_its_supervisor_and_to_no_other_agent() {
    let app = app_21().await;
    // `w1_dup` is an agent; `w1_dup-a` is a worker of that agent, never of `w1`.
    for name in [
        "other-a", "w1-", "w1_a", "w1", "w1_dup", "w1_dup-a", "w1-a-b",
    ] {
        let (status, _) = call(
            &app,
            "/v1/work/workers",
            Some("tok-w1"),
            json!({ "worker": name }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
    }
    // A person token is not a supervisor; an ingest-free app has no third kind.
    let (status, _) = call(
        &app,
        "/v1/work/workers",
        Some("tok-gab"),
        json!({ "worker": "gab-a" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

fn worker(name: &str) -> crate::tracker::NewWorker {
    crate::tracker::NewWorker {
        worker: AgentName::agent_or_worker(name).unwrap(),
        vendor: None,
        profile: None,
        model: None,
    }
}

/// A team whose token file gives `w1` the token `tok-w1`, with one worker
/// started under that token and one started under a token `w1` had before.
async fn teams_with_two_workers() -> (Arc<Teams<Quiet>>, String, String) {
    let felix = Quiet::default();
    felix.keeps.store(true, Ordering::SeqCst);
    let tokens = r#"{"agents":{"tok-w1":"w1"},"people":{"tok-gab":"gab"},"ingest":{}}"#;
    let one = team("default", felix, tokens, 64).await;
    let teams = Arc::new(Teams::new(vec![one]).unwrap());
    let app = router(teams.clone());
    raise_to(&app, 21).await;
    let t = teams.get("default").unwrap().tracker().unwrap();
    let sup = AgentName::new("w1").unwrap();
    let now = t
        .start_worker(&sup, &TokenHash::of("tok-w1"), worker("w1-now"))
        .await
        .unwrap();
    let old = t
        .start_worker(&sup, &TokenHash::of("tok-w1-old"), worker("w1-old"))
        .await
        .unwrap();
    let tok = |v: &Value| v["token"].as_str().unwrap().to_owned();
    (teams, tok(&now), tok(&old))
}

#[tokio::test]
async fn a_worker_does_not_come_back_with_a_supervisor_name_given_another_token() {
    let (teams, now, old) = teams_with_two_workers().await;
    let found = teams.who(&now).unwrap();
    assert_eq!(found.len(), 1);
    assert!(matches!(&found[0].1, Caller::Agent(a) if a.as_str() == "w1-now"));
    // `w1` now has another token: the worker it started before is not its.
    assert!(teams.who(&old).unwrap().is_empty());
}

#[tokio::test]
async fn the_workers_of_a_gone_supervisor_identity_are_stopped() {
    let (teams, now, old) = teams_with_two_workers().await;
    let team = teams.get("default").unwrap();
    let t = team.tracker().unwrap();
    let stopped = crate::teams::stop_orphans(&team, None, &t).await;
    assert!(stopped.is_ok());
    let listed = t.workers(None).await;
    let stopped_ms = |n: &str| {
        listed["workers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["worker"] == n)
            .unwrap()["stopped_ms"]
            .clone()
    };
    assert!(stopped_ms("w1-old").is_u64());
    assert!(stopped_ms("w1-now").is_null());
    assert!(t.worker_by_token(&old).is_none());
    assert!(t.worker_by_token(&now).is_some());
}

#[tokio::test]
async fn a_worker_reports_it_finished_with_its_own_token_and_only_once() {
    let app = app_21().await;
    raise_to(&app, 24).await;
    let start = json!({ "worker": "w1-a" });
    let (_, reply) = call(&app, "/v1/work/workers", Some("tok-w1"), start).await;
    let tok = reply["token"].as_str().unwrap().to_owned();
    let none = json!({});
    // Not a supervisor's call, not a person's, and not an agent without workers.
    for token in ["tok-w1", "tok-gab"] {
        let (status, _) = call(&app, "/v1/work/workers/finish", Some(token), none.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{token}");
    }
    let (status, _) = call(&app, "/v1/work/workers/finish", None, none.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = call(&app, "/v1/work/workers/finish", Some(&tok), none.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, listed) = get_json(&app, "/v1/work/workers", Some("tok-w1")).await;
    assert_eq!(listed["workers"][0]["finished"], true);
    // The token died with it.
    let (status, _) = call(&app, "/v1/work/workers/finish", Some(&tok), none).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &app,
        "/v1/work/next",
        Some(&tok),
        json!({ "wait_seconds": 0 }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_person_stops_any_worker_and_the_list_shows_it_stopped() {
    let app = app_21().await;
    let (_, reply) = call(
        &app,
        "/v1/work/workers",
        Some("tok-w1"),
        json!({ "worker": "w1-a" }),
    )
    .await;
    let tok = reply["token"].as_str().unwrap().to_owned();
    // The dashboard's Stop control: a person's token, the worker's name.
    let stop = json!({ "worker": "w1-a" });
    let (status, body) = call(&app, "/v1/work/workers/stop", Some("tok-gab"), stop.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["kind"], "worker_stopped");
    let (_, all) = get_json(&app, "/v1/work/workers", Some("tok-gab")).await;
    assert!(all["workers"][0]["stopped_ms"].is_u64(), "{all}");
    assert_eq!(all["workers"][0]["finished"], false);
    let next = json!({ "wait_seconds": 0 });
    let (status, _) = call(&app, "/v1/work/next", Some(&tok), next).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A second stop is refused, which a Stop all that races one reads as already done.
    let (status, again) = call(&app, "/v1/work/workers/stop", Some("tok-gab"), stop).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(again["why"], "bad_worker", "{again}");
}
