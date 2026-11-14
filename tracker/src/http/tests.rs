use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::body::Body;
use axum::http::Request;
use kbl_core::org::TeamName;
use tower::ServiceExt;

use super::*;
use crate::config::Tokens;

/// A Felix with no records that remembers which streams were published to, and
/// whose writer lease can be taken away.
#[derive(Clone, Default)]
struct Quiet {
    published: Arc<StdMutex<Vec<String>>>,
    lease_lost: Arc<AtomicBool>,
}

impl Quiet {
    fn published(&self) -> Vec<String> {
        self.published.lock().unwrap().clone()
    }
}

impl Broker for Quiet {
    async fn publish(&self, stream: &str, _: Vec<u8>) -> anyhow::Result<u64> {
        let mut published = self.published.lock().unwrap();
        published.push(stream.to_owned());
        Ok(published.len() as u64 - 1)
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
    async fn nack(&self, _: &str, _: &str, _: &felix_client::GroupRecord) -> anyhow::Result<()> {
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
        Ok((!self.lease_lost.load(Ordering::SeqCst)).then_some(1))
    }
    async fn forget(&self, _: Option<&str>) {}
    async fn close_idle(&self, _: Duration) {}
}

async fn team(name: &str, broker: Quiet, tokens: &str, inflight: usize) -> Team<Quiet> {
    let tracker = Tracker::start(
        broker,
        crate::tracker::Settings {
            grants: Default::default(),
            visibility: Duration::from_secs(90),
            lease_ttl: Duration::from_secs(15),
            lease_wait: Duration::ZERO,
            crash_after_poll: false,
            review_tiers: crate::tracker::default_review_tiers(),
        },
    )
    .await
    .unwrap();
    Team::new(
        TeamName::new(name).unwrap(),
        tracker,
        Tokens::parse(tokens).unwrap(),
        inflight,
    )
}

/// A tracker with one team and an empty Felix: enough to exercise auth and routing.
async fn app() -> Router {
    let tokens =
        r#"{"agents":{"tok-w1":"w1"},"people":{"tok-gab":"gab"},"ingest":{"tok-ing":"ingest-gh"}}"#;
    let one = team("default", Quiet::default(), tokens, 64).await;
    router(Arc::new(Teams::new(vec![one]).unwrap()))
}

/// Two teams, `a` and `b`, each with its own Felix. `tok-both` is one person in both.
struct Two {
    app: Router,
    teams: Arc<Teams<Quiet>>,
    a: Quiet,
    b: Quiet,
}

async fn two(inflight_a: usize) -> Two {
    let (a, b) = (Quiet::default(), Quiet::default());
    let ta = team(
        "a",
        a.clone(),
        r#"{"agents":{"tok-a-w1":"w1"},"people":{"tok-a-gab":"gab","tok-both":"kim"},"ingest":{"tok-a-ing":"ing"}}"#,
        inflight_a,
    )
    .await;
    let tb = team(
        "b",
        b.clone(),
        r#"{"agents":{"tok-b-w1":"w1"},"people":{"tok-b-ana":"ana","tok-both":"kim"}}"#,
        64,
    )
    .await;
    let teams = Arc::new(Teams::new(vec![ta, tb]).unwrap());
    Two {
        app: router(teams.clone()),
        teams,
        a,
        b,
    }
}

fn imported(item: &str) -> Value {
    json!({ "record": { "kind": "imported", "item": item, "source": "gh", "upstream_id": item, "title": "t" } })
}

async fn call(app: &Router, path: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    send(app, Request::post(path), token, body).await
}

async fn get_json(app: &Router, path: &str, token: Option<&str>) -> (StatusCode, Value) {
    send(app, Request::get(path), token, Value::Null).await
}

async fn send(
    app: &Router,
    req: axum::http::request::Builder,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = req.header("content-type", "application/json");
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
            "/v1/work/triage",
            json!({ "item": "a1", "tier": 1, "why": "w" }),
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
async fn an_agent_token_cannot_move_work_to_another_repo() {
    let app = app().await;
    let body = json!({
        "item": "a1", "question": 1, "text": { "text": "x" },
        "moves_to": { "source": "gh", "repo": "o/other", "title": "t" }
    });
    let (status, _) = call(&app, "/v1/work/answer", Some("tok-w1"), body).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn only_a_person_token_sets_rules() {
    let app = app().await;
    let body = json!({ "source": "github", "repo": "GetFelix/felix", "text": "one worktree", "checks": ["task lint"] });
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

#[tokio::test]
async fn a_token_acts_only_in_its_own_team() {
    let two = two(64).await;
    let app = &two.app;
    // The token's own team, with and without the team in the path.
    for (path, item) in [
        ("/v1/work/records", "a1"),
        ("/v1/teams/a/work/records", "a2"),
    ] {
        assert_eq!(
            call(app, path, Some("tok-a-gab"), imported(item)).await.0,
            StatusCode::OK,
            "{path}"
        );
    }
    assert_eq!(two.a.published(), ["work.events", "work.events"]);
    // Another team's path answers as a team that does not exist, and writes nothing there.
    for (path, token) in [
        ("/v1/teams/b/work/records", "tok-a-gab"),
        ("/v1/teams/b/work/next", "tok-a-w1"),
        ("/v1/teams/b/ingest/records", "tok-a-ing"),
        ("/v1/teams/nope/work/records", "tok-a-gab"),
    ] {
        let (status, reply) = call(app, path, Some(token), imported("a1")).await;
        assert_eq!(
            (status, reply["why"].as_str()),
            (StatusCode::NOT_FOUND, Some("unknown_team")),
            "{path} with {token}"
        );
    }
    assert!(two.b.published().is_empty());
    assert_eq!(two.a.published().len(), 2);
    // A token is checked before the team.
    assert_eq!(
        call(
            app,
            "/v1/teams/b/work/records",
            Some("nope"),
            imported("a1")
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    // Only the work and ingest routes have a team form.
    assert_eq!(
        get_json(app, "/v1/teams/a/me", Some("tok-a-gab")).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_person_in_two_teams_names_the_team() {
    let two = two(64).await;
    let (status, reply) = call(
        &two.app,
        "/v1/work/records",
        Some("tok-both"),
        imported("a1"),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::BAD_REQUEST, Some("team_required"))
    );
    assert!(two.a.published().is_empty() && two.b.published().is_empty());
    let (status, _) = call(
        &two.app,
        "/v1/teams/b/work/records",
        Some("tok-both"),
        imported("a1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(two.b.published(), ["work.events"]);
    assert!(two.a.published().is_empty());
}

#[tokio::test]
async fn me_lists_the_persons_own_teams() {
    let two = two(64).await;
    let (status, reply) = get_json(&two.app, "/v1/me", Some("tok-b-ana")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["person"], "ana");
    assert_eq!(reply["teams"], json!([{ "team": "b", "role": "admin" }]));
    let (_, reply) = get_json(&two.app, "/v1/me", Some("tok-both")).await;
    let names: Vec<&str> = reply["teams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["team"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a", "b"]);
    assert_eq!(
        get_json(&two.app, "/v1/me", Some("tok-a-w1")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get_json(&two.app, "/v1/me", None).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_busy_team_does_not_hold_up_another() {
    let two = two(1).await;
    let busy = two.teams.get("a").unwrap().admit(false).unwrap();
    let (status, reply) = call(
        &two.app,
        "/v1/work/records",
        Some("tok-a-gab"),
        imported("a1"),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("team_busy"))
    );
    assert!(two.a.published().is_empty());
    let (status, _) = call(
        &two.app,
        "/v1/work/records",
        Some("tok-b-ana"),
        imported("a1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    drop(busy);
    let (status, _) = call(
        &two.app,
        "/v1/work/records",
        Some("tok-a-gab"),
        imported("a1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn a_waiting_work_next_keeps_its_slot_and_not_the_others() {
    let two = two(1).await;
    let app = two.app.clone();
    let waiting = tokio::spawn(async move {
        call(
            &app,
            "/v1/work/next",
            Some("tok-a-w1"),
            json!({ "wait_seconds": 5 }),
        )
        .await
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (status, reply) = call(
        &two.app,
        "/v1/work/next",
        Some("tok-a-w1"),
        json!({ "wait_seconds": 0 }),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("team_busy"))
    );
    // A holder's calls are not behind the waiting poll.
    let (status, reply) = call(
        &two.app,
        "/v1/work/extend",
        Some("tok-a-w1"),
        json!({ "item": "a1" }),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("lost"))
    );
    assert_eq!(waiting.await.unwrap().0, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn a_team_that_loses_its_lease_answers_503_alone() {
    let two = two(64).await;
    let teams = two.teams.clone();
    let maintain = tokio::spawn(async move { teams.maintain().await });
    two.b.lease_lost.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(60)).await;
    let (status, _) = call(
        &two.app,
        "/v1/work/records",
        Some("tok-b-ana"),
        imported("a1"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = call(
        &two.app,
        "/v1/work/records",
        Some("tok-a-gab"),
        imported("a1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !maintain.is_finished(),
        "one team's lost lease stopped the tracker"
    );
    // With no team left holding its lease, the tracker stops.
    two.a.lease_lost.store(true, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(60), maintain)
        .await
        .expect("maintenance ends when every team lost its lease")
        .unwrap();
}

#[tokio::test]
async fn a_token_names_one_caller_across_teams() {
    let agent = r#"{"agents":{"tok-x":"w1"}}"#;
    let person = |name: &str| format!(r#"{{"people":{{"tok-x":"{name}"}}}}"#);
    for (a, b, ok) in [
        (agent.to_owned(), agent.to_owned(), false),
        (agent.to_owned(), person("kim"), false),
        (person("kim"), person("ana"), false),
        (person("kim"), person("kim"), true),
    ] {
        let teams = vec![
            team("a", Quiet::default(), &a, 64).await,
            team("b", Quiet::default(), &b, 64).await,
        ];
        assert_eq!(Teams::new(teams).is_ok(), ok, "{a} and {b}");
    }
    let twice = vec![
        team("a", Quiet::default(), "{}", 64).await,
        team("a", Quiet::default(), "{}", 64).await,
    ];
    assert!(Teams::new(twice).is_err());
}
