use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

use super::*;

/// A tracker whose Felix is empty: enough to exercise auth and routing.
async fn app() -> Router {
    #[derive(Clone)]
    struct Quiet;
    impl Broker for Quiet {
        async fn publish(&self, _: &str, _: Vec<u8>) -> anyhow::Result<u64> {
            Ok(0)
        }
        async fn read_from(&self, _: &str, _: u64) -> anyhow::Result<Vec<(u64, Vec<u8>)>> {
            Ok(Vec::new())
        }
        async fn poll(
            &self,
            _: &str,
            _: &str,
            _: Duration,
        ) -> anyhow::Result<Option<felix_client::GroupRecord>> {
            Ok(None)
        }
        async fn ack(&self, _: &str, _: &str, _: u64) -> anyhow::Result<()> {
            Ok(())
        }
        async fn nack(
            &self,
            _: &str,
            _: &str,
            _: &felix_client::GroupRecord,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn extend(
            &self,
            _: &str,
            _: &str,
            _: &felix_client::GroupRecord,
            by: Duration,
        ) -> Result<Duration, crate::felix::ExtendError> {
            Ok(by)
        }
        async fn lease_put(
            &self,
            _: Vec<u8>,
            _: Duration,
            _: Option<u64>,
        ) -> anyhow::Result<Option<u64>> {
            Ok(Some(1))
        }
        async fn forget(&self, _: Option<&str>) {}
        async fn close_idle(&self, _: Duration) {}
    }
    let tokens = Tokens::parse(
        r#"{"agents":{"tok-w1":"w1"},"people":{"tok-gab":"gab"},"ingest":{"tok-ing":"ingest-gh"}}"#,
    )
    .unwrap();
    let tracker = Tracker::start(
        Quiet,
        crate::tracker::Settings {
            grants: Default::default(),
            visibility: Duration::from_secs(90),
            lease_ttl: Duration::from_secs(15),
            lease_wait: Duration::ZERO,
            crash_after_poll: false,
        },
    )
    .await
    .unwrap();
    router(tracker, tokens)
}

async fn call(app: &Router, path: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut req = Request::post(path).header("content-type", "application/json");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn a_call_without_a_known_token_is_refused() {
    let app = app().await;
    let body = json!({ "wait_seconds": 0 });
    assert_eq!(
        call(&app, "/v1/work/next", None, body.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "/v1/work/next", Some("nope"), body).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn an_agent_token_claims_and_a_person_token_does_not() {
    let app = app().await;
    let body = json!({ "wait_seconds": 0 });
    let (status, reply) = call(&app, "/v1/work/next", Some("tok-w1"), body.clone()).await;
    assert_eq!(
        (status, reply["claimed"].as_bool()),
        (StatusCode::OK, Some(false))
    );
    assert_eq!(
        call(&app, "/v1/work/next", Some("tok-gab"), body).await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn only_a_person_token_appends_records() {
    let app = app().await;
    let record = json!({ "record": { "kind": "imported", "item": "a1", "source": "gh", "upstream_id": "1", "title": "t" } });
    assert_eq!(
        call(&app, "/v1/work/records", Some("tok-w1"), record.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, reply) = call(&app, "/v1/work/records", Some("tok-gab"), record).await;
    assert_eq!(
        (status, reply["kind"].as_str()),
        (StatusCode::OK, Some("imported"))
    );
}

#[tokio::test]
async fn a_lost_claim_is_a_conflict_with_a_reason() {
    let app = app().await;
    let (status, reply) = call(
        &app,
        "/v1/work/extend",
        Some("tok-w1"),
        json!({ "item": "a1" }),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("lost"))
    );
}

#[tokio::test]
async fn the_ingest_token_writes_sync_records_and_nothing_else() {
    let app = app().await;
    let record = json!({ "record": { "kind": "imported", "item": "a1", "source": "gh", "upstream_id": "1", "title": "t" } });
    for token in ["tok-w1", "tok-gab"] {
        assert_eq!(
            call(&app, "/v1/ingest/records", Some(token), record.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let (status, reply) = call(&app, "/v1/ingest/records", Some("tok-ing"), record).await;
    assert_eq!(
        (status, reply["kind"].as_str()),
        (StatusCode::OK, Some("imported"))
    );
    let claim = json!({ "wait_seconds": 0 });
    assert_eq!(
        call(&app, "/v1/work/next", Some("tok-ing"), claim).await.0,
        StatusCode::FORBIDDEN
    );
    let budget = json!({ "record": { "kind": "budget_set", "root": "a1", "items_left": 9, "max_depth": 3, "cost_left_micros": 1, "redos_per_item": 3 } });
    let (status, reply) = call(&app, "/v1/ingest/records", Some("tok-ing"), budget).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("not_allowed"))
    );
}

#[tokio::test]
async fn the_work_tools_are_for_agents_and_the_dashboard_routes_for_people() {
    let app = app().await;
    let agent_routes = [
        (
            "/v1/work/post",
            json!({ "item": "a1", "key": "k", "work": "split", "title": "t" }),
        ),
        ("/v1/work/link", json!({ "item": "a1", "to": "a2" })),
        ("/v1/work/context", json!({ "item": "a1" })),
        (
            "/v1/work/context/add",
            json!({ "item": "a1", "entry": "e", "text": { "text": "x" } }),
        ),
        (
            "/v1/work/progress",
            json!({ "item": "a1", "note": { "text": "x" } }),
        ),
        (
            "/v1/work/step",
            json!({ "item": "a1", "command": "c", "exit_code": 0, "output": { "text": "x" } }),
        ),
        (
            "/v1/work/edit",
            json!({ "item": "a1", "file": "f", "hunk": { "text": "x" }, "decision": 1 }),
        ),
        (
            "/v1/work/ask",
            json!({ "item": "a1", "text": { "text": "x" } }),
        ),
    ];
    for (path, body) in agent_routes {
        assert_eq!(
            call(&app, path, Some("tok-gab"), body.clone()).await.0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
        assert_eq!(
            call(&app, path, None, body).await.0,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
    let person_routes = [
        ("/v1/work/proposals/accept", json!({ "item": "a1" })),
        (
            "/v1/work/proposals/edit",
            json!({ "item": "a1", "tier": 1 }),
        ),
        ("/v1/work/proposals/reject", json!({ "item": "a1" })),
        (
            "/v1/work/questions/answer",
            json!({ "item": "a1", "text": { "text": "x" } }),
        ),
        ("/v1/work/tier", json!({ "item": "a1", "tier": 1 })),
        ("/v1/work/close", json!({ "item": "a1" })),
        ("/v1/work/unlink", json!({ "item": "a1", "to": "a2" })),
        (
            "/v1/work/budget",
            json!({ "root": "a1", "items_left": 1, "max_depth": 1, "cost_left_micros": 1, "redos_per_item": 1 }),
        ),
    ];
    for (path, body) in person_routes {
        assert_eq!(
            call(&app, path, Some("tok-w1"), body.clone()).await.0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
        assert_eq!(
            call(&app, path, None, body).await.0,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_decision_without_a_phase_or_a_commit_never_reaches_the_tracker() {
    let app = app().await;
    let full = json!({
        "item": "a1", "phase": "cause", "so_far": "s", "why": "w", "commit": "c",
        "evidence": [{ "stream": "work.events", "offset": 0 }]
    });
    // No phase: the body does not parse.
    let mut no_phase = full.clone();
    no_phase.as_object_mut().unwrap().remove("phase");
    assert_eq!(
        call(&app, "/v1/work/decide", Some("tok-w1"), no_phase)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut no_commit = full.clone();
    no_commit.as_object_mut().unwrap().remove("commit");
    assert_eq!(
        call(&app, "/v1/work/decide", Some("tok-w1"), no_commit)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut bad_phase = full.clone();
    bad_phase["phase"] = json!("guess");
    assert_eq!(
        call(&app, "/v1/work/decide", Some("tok-w1"), bad_phase)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // With every field it parses, and then the tracker says the agent holds nothing.
    let (status, reply) = call(&app, "/v1/work/decide", Some("tok-w1"), full).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("not_held"))
    );
}

#[tokio::test]
async fn an_unknown_kind_of_work_is_a_bad_request() {
    let app = app().await;
    let (status, reply) = call(
        &app,
        "/v1/work/post",
        Some("tok-w1"),
        json!({ "item": "a1", "key": "k", "work": "triage", "title": "t" }),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid"))
    );
}

#[tokio::test]
async fn a_prerequisite_is_an_agent_call_with_one_of_two_shapes() {
    let app = app().await;
    let new = json!({
        "item": "a1", "key": "k", "work": "prerequisite", "title": "t",
        "target_source": "github", "target_repo": "o/r", "why": { "text": "w" }
    });
    // People post nothing on an agent's behalf.
    assert_eq!(
        call(&app, "/v1/work/post", Some("tok-gab"), new.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // Nor can anyone write the record through the person or ingest routes.
    let record = json!({ "record": {
        "kind": "prerequisite", "item": "a1", "attempt": 1, "prerequisite": "g1", "key": "k"
    } });
    for (path, token) in [
        ("/v1/work/records", "tok-gab"),
        ("/v1/ingest/records", "tok-ing"),
    ] {
        let (status, _) = call(&app, path, Some(token), record.clone()).await;
        assert_ne!(status, StatusCode::OK);
    }
    // Mixing the shapes, or leaving out the reason, never reaches the tracker.
    for bad in [
        json!({ "item": "a1", "key": "k", "work": "prerequisite", "title": "t", "existing": "g1" }),
        json!({ "item": "a1", "key": "k", "work": "prerequisite", "title": "t", "target_source": "github", "target_repo": "o/r" }),
    ] {
        let (status, reply) = call(&app, "/v1/work/post", Some("tok-w1"), bad).await;
        assert_eq!(
            (status, reply["why"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid"))
        );
    }
    // A well-formed one reaches the holder check.
    let (status, reply) = call(&app, "/v1/work/post", Some("tok-w1"), new).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("not_held"))
    );
}

#[tokio::test]
async fn only_a_person_token_sets_rules() {
    let app = app().await;
    let body = json!({ "source": "github", "repo": "GetFelix/felix", "text": { "text": "one worktree" }, "checks": ["task lint"] });
    for token in ["tok-w1", "tok-ing"] {
        assert_eq!(
            call(&app, "/v1/work/rules", Some(token), body.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let (status, reply) = call(&app, "/v1/work/rules", Some("tok-gab"), body).await;
    assert_eq!(
        (status, reply["kind"].as_str()),
        (StatusCode::OK, Some("rules_set"))
    );
}
