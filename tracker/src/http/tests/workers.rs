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
    let start = json!({ "worker": "w1_a", "vendor": "fake", "profile": "fake", "model": "small" });
    let (status, reply) = call(&app, "/v1/work/workers", Some("tok-w1"), start).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let tok = reply["token"].as_str().unwrap().to_owned();
    let next = json!({ "wait_seconds": 0 });
    let (status, _) = call(&app, "/v1/work/next", Some(&tok), next.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (_, listed) = get_json(&app, "/v1/work/workers", Some("tok-w1")).await;
    assert_eq!(listed["workers"][0]["worker"], "w1_a");
    let (_, all) = get_json(&app, "/v1/work/workers", Some("tok-gab")).await;
    assert_eq!(all["workers"].as_array().unwrap().len(), 1);
    // A worker cannot start workers of its own.
    let nested = json!({ "worker": "w1_a_x" });
    let (status, _) = call(&app, "/v1/work/workers", Some(&tok), nested).await;
    assert_ne!(status, StatusCode::OK);
    let stop = json!({ "worker": "w1_a" });
    let (status, _) = call(&app, "/v1/work/workers/stop", Some("tok-w1"), stop).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "/v1/work/next", Some(&tok), next).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_worker_name_belongs_to_its_supervisor_and_to_no_other_agent() {
    let app = app_21().await;
    for name in ["other_a", "w1_", "w1-a", "w1"] {
        let (status, _) = call(
            &app,
            "/v1/work/workers",
            Some("tok-w1"),
            json!({ "worker": name }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
    }
    let (status, reply) = call(
        &app,
        "/v1/work/workers",
        Some("tok-w1"),
        json!({ "worker": "w1_dup" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{reply}");
    // A person token is not a supervisor; an ingest-free app has no third kind.
    let (status, _) = call(
        &app,
        "/v1/work/workers",
        Some("tok-gab"),
        json!({ "worker": "gab_a" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
