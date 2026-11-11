//! The REST API. Every call carries a bearer token that the token file maps to
//! an agent or a person; the name on each record comes from there, never from
//! the body.

use std::sync::Arc;
use std::time::Duration;

use agents_core::names::{AgentName, ItemId, Key};
use agents_core::work::WorkRecord;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{Caller, Tokens};
use crate::felix::Broker;
use crate::tracker::{Error, Tracker};

mod ingest;

/// A call the token does not allow: status and why.
type Denied = (StatusCode, &'static str);

struct App<B> {
    tracker: Tracker<B>,
    tokens: Tokens,
}

pub(crate) fn router<B: Broker>(tracker: Tracker<B>, tokens: Tokens) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/work/next", post(next::<B>))
        .route("/v1/work/extend", post(extend::<B>))
        .route("/v1/work/complete", post(complete::<B>))
        .route("/v1/work/release", post(release::<B>))
        .route("/v1/work/give_up", post(give_up::<B>))
        .route("/v1/work/records", post(records::<B>))
        .merge(ingest::routes::<B>())
        .with_state(Arc::new(App { tracker, tokens }))
}

#[derive(Deserialize)]
struct NextBody {
    wait_seconds: Option<u64>,
}

#[derive(Deserialize)]
struct ItemBody {
    item: ItemId,
}

#[derive(Deserialize)]
struct CompleteBody {
    item: ItemId,
    #[serde(default)]
    cost_micros: u64,
}

#[derive(Deserialize)]
struct ReleaseBody {
    item: ItemId,
    reason: Option<String>,
    #[serde(default)]
    cost_micros: u64,
}

#[derive(Deserialize)]
struct GiveUpBody {
    item: ItemId,
    reason: String,
    #[serde(default)]
    cost_micros: u64,
}

#[derive(Deserialize)]
struct RecordBody {
    record: WorkRecord,
}

async fn next<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(body): Json<NextBody>,
) -> Response {
    let agent = match agent(&app, &headers) {
        Ok(a) => a,
        Err(d) => return denied(d),
    };
    let wait = body
        .wait_seconds
        .map_or(Duration::from_secs(10), Duration::from_secs);
    reply(app.tracker.next(&agent, wait).await)
}

async fn extend<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(body): Json<ItemBody>,
) -> Response {
    match agent(&app, &headers) {
        Ok(a) => reply(app.tracker.extend(&a, &body.item).await),
        Err(d) => denied(d),
    }
}

async fn complete<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(body): Json<CompleteBody>,
) -> Response {
    match agent(&app, &headers) {
        Ok(a) => reply(app.tracker.complete(&a, &body.item, body.cost_micros).await),
        Err(d) => denied(d),
    }
}

async fn release<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(body): Json<ReleaseBody>,
) -> Response {
    match agent(&app, &headers) {
        Ok(a) => reply(
            app.tracker
                .release(&a, &body.item, body.reason, body.cost_micros)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn give_up<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(body): Json<GiveUpBody>,
) -> Response {
    match agent(&app, &headers) {
        Ok(a) => reply(
            app.tracker
                .give_up(&a, &body.item, body.reason, body.cost_micros)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn records<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(body): Json<RecordBody>,
) -> Response {
    match person(&app, &headers) {
        Ok(p) => reply(app.tracker.person_record(p, body.record).await),
        Err(d) => denied(d),
    }
}

fn caller<B>(app: &App<B>, headers: &HeaderMap) -> Result<Caller, Denied> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    token
        .and_then(|t| app.tokens.who(t))
        .ok_or((StatusCode::UNAUTHORIZED, "unknown or missing token"))
}

fn agent<B>(app: &App<B>, headers: &HeaderMap) -> Result<AgentName, Denied> {
    match caller(app, headers)? {
        Caller::Agent(a) => Ok(a),
        Caller::Person(_) | Caller::Ingest(_) => {
            Err((StatusCode::FORBIDDEN, "this call is for an agent's token"))
        }
    }
}

fn person<B>(app: &App<B>, headers: &HeaderMap) -> Result<Key, Denied> {
    match caller(app, headers)? {
        Caller::Person(p) => Ok(p),
        Caller::Agent(_) | Caller::Ingest(_) => {
            Err((StatusCode::FORBIDDEN, "this call is for a person's token"))
        }
    }
}

fn reply(result: Result<Value, Error>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => {
            let (status, extra) = match &e {
                Error::Refused { why } => (StatusCode::CONFLICT, json!({ "why": why })),
                Error::NotHeld(_) => (StatusCode::CONFLICT, json!({ "why": "not_held" })),
                Error::Lost(_) => (StatusCode::CONFLICT, json!({ "why": "lost" })),
                Error::Felix(_) => (StatusCode::BAD_GATEWAY, json!({})),
                Error::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, json!({})),
            };
            let mut body = json!({ "error": e.to_string() });
            if let (Some(b), Some(x)) = (body.as_object_mut(), extra.as_object()) {
                b.extend(x.clone());
            }
            (status, Json(body)).into_response()
        }
    }
}

fn denied((status, message): Denied) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[cfg(test)]
mod tests;
