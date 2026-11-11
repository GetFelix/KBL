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
        async fn ack(&self, _: &str, _: u64) -> anyhow::Result<()> {
            Ok(())
        }
        async fn nack(&self, _: &str, _: &felix_client::GroupRecord) -> anyhow::Result<()> {
            Ok(())
        }
        async fn extend(
            &self,
            _: &str,
            _: &felix_client::GroupRecord,
            by: Duration,
        ) -> Result<Duration, crate::felix::ExtendError> {
            Ok(by)
        }
    }
    let tokens = Tokens::parse(r#"{"agents":{"tok-w1":"w1"},"people":{"tok-gab":"gab"}}"#).unwrap();
    let tracker = Tracker::start(Quiet, Duration::from_secs(90))
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
