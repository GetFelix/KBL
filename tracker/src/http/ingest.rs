//! The calls only the ingest worker makes: the upstream sync records, the
//! roots it imported, its saved state, and the config the org log holds for
//! its worker group. Its token cannot claim work or write the records a
//! person writes.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use kbl_core::names::Key;
use kbl_core::org::{TeamName, TokenHash};
use kbl_core::org_fold::OrgState;
use kbl_core::work::WorkRecord;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{App, Call, Caller, StatusCode, denied, refused, reply};
use crate::config::Worker;
use crate::felix::Broker;

pub(super) fn routes<B: Broker>() -> Router<Arc<App<B>>> {
    Router::new()
        .route("/v1/ingest/records", post(record::<B>))
        .route("/v1/ingest/roots", get(roots::<B>))
        .route("/v1/ingest/state/{source}", get(load::<B>).put(save::<B>))
        .route("/v1/ingest/config", get(config::<B>))
}

/// Config long polls held at once across the tracker. One per worker; well
/// past the 200 workers the design is sized for.
pub(super) const MAX_HELD_POLLS: usize = 1024;

/// The longest a config poll is held, as `wait` asks.
const MAX_WAIT: Duration = Duration::from_secs(60);

#[derive(Deserialize)]
struct ConfigQuery {
    group: String,
    #[serde(default)]
    wait: u64,
}

#[derive(Deserialize)]
struct RecordBody {
    record: WorkRecord,
}

#[derive(Deserialize)]
struct SaveBody {
    state: serde_json::Value,
    /// The version the state was read at; none when it was absent.
    version: Option<u64>,
}

const STATE_OF_OTHER: super::Denied = (
    StatusCode::FORBIDDEN,
    "this ingest token keeps another source's state",
);

impl<B> Call<B> {
    fn worker(&self) -> Result<Worker, super::Denied> {
        match &self.caller {
            Caller::Ingest(w) => Ok(w.clone()),
            _ => Err((StatusCode::FORBIDDEN, "this call is for an ingest token")),
        }
    }
}

async fn record<B: Broker>(call: Call<B>, Json(body): Json<RecordBody>) -> Response {
    match call.worker() {
        Ok(w) => reply(
            call.tracker()
                .ingest_record_in(w.key, w.source.as_deref(), body.record)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn roots<B: Broker>(call: Call<B>) -> Response {
    match call.worker() {
        Ok(w) => reply(call.tracker().ingest_roots(w.source.as_deref()).await),
        Err(d) => denied(d),
    }
}

async fn load<B: Broker>(call: Call<B>, Path(source): Path<String>) -> Response {
    match call.worker() {
        Ok(w) if !w.may_keep(&source) => denied(STATE_OF_OTHER),
        Ok(_) => reply(call.tracker().ingest_state(&source).await),
        Err(d) => denied(d),
    }
}

async fn save<B: Broker>(
    call: Call<B>,
    Path(source): Path<String>,
    Json(body): Json<SaveBody>,
) -> Response {
    match call.worker() {
        Ok(w) if !w.may_keep(&source) => denied(STATE_OF_OTHER),
        Ok(_) => reply(
            call.tracker()
                .ingest_save(&source, &body.state, body.version)
                .await,
        ),
        Err(d) => denied(d),
    }
}

/// `GET /v1/ingest/config?group=&wait=`: the connection the org log gives the
/// worker's group in its team, the sources it serves and the pool's budget.
/// With `If-None-Match` set to the last `ETag`, the call is held until the
/// answer changes or `wait` seconds pass, then answers `304`.
async fn config<B: Broker>(
    State(app): State<Arc<App<B>>>,
    call: Call<B>,
    Query(q): Query<ConfigQuery>,
    headers: HeaderMap,
) -> Response {
    let worker = match call.worker() {
        Ok(w) => w,
        Err(d) => return denied(d),
    };
    let Ok(group) = Key::new(q.group) else {
        return refused(StatusCode::BAD_REQUEST, "group is a key", "invalid");
    };
    let Some(org) = call.teams.org().map(|o| o.org.clone()) else {
        return refused(
            StatusCode::NOT_FOUND,
            "this tracker runs without teams: set KBL_ORG_NAMESPACE",
            "teams_off",
        );
    };
    let team = call.team.name().clone();
    // A held poll must not keep one of the team's call slots for a minute.
    let Call { _slot, .. } = call;
    drop(_slot);
    let seen = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let wait = Duration::from_secs(q.wait).min(MAX_WAIT);
    let held = match seen {
        Some(_) if !wait.is_zero() => app.polls.clone().try_acquire_owned().ok(),
        _ => None,
    };
    let mut changes = org.watch();
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        changes.borrow_and_update();
        let (body, etag) = org.with_view(|state, _| {
            let body = config_answer(state, &team, &group, worker.source.as_deref());
            let etag = etag_of(&body);
            (body, etag)
        });
        if seen.as_deref() != Some(etag.as_str()) {
            let mut res = Json(body).into_response();
            if let Ok(v) = HeaderValue::from_str(&etag) {
                res.headers_mut().insert(header::ETAG, v);
            }
            return res;
        }
        if held.is_none()
            || tokio::time::timeout_at(deadline, changes.changed())
                .await
                .is_err()
        {
            let mut res = StatusCode::NOT_MODIFIED.into_response();
            if let Ok(v) = HeaderValue::from_str(&etag) {
                res.headers_mut().insert(header::ETAG, v);
            }
            return res;
        }
    }
}

/// The answer for `group` in `team`. A group no connection names gets
/// `connection: null`, and its worker stays file-managed. A token tied to a
/// vendor sees only a connection to that vendor.
fn config_answer(state: &OrgState, team: &TeamName, group: &Key, tied: Option<&str>) -> Value {
    let conn = state
        .connection_of_group(team, group)
        .filter(|(_, c)| tied.is_none_or(|v| v == c.vendor));
    let Some((name, c)) = conn else {
        return json!({ "org_format": state.format(), "connection": null, "budget": null });
    };
    let budget = state.budget(&c.pool).map(|b| {
        // The polling share of the pool, by this connection's part of its sources.
        let polling = b.per_hour - b.kept_back;
        let total = state.pool_sources(&c.pool).max(1) as u64;
        json!({
            "per_hour": b.per_hour,
            "kept_back": b.kept_back,
            "wake_cap": b.wake_cap,
            "share_per_hour": polling.saturating_mul(c.sources.len() as u64) / total,
        })
    });
    json!({
        "org_format": state.format(),
        "connection": {
            "name": name,
            "vendor": c.vendor,
            "group": c.group,
            "pool": c.pool,
            "sources": c.sources,
        },
        "budget": budget,
    })
}

/// A digest of the answer, so a record for another team or group leaves a
/// worker's poll held.
fn etag_of(body: &Value) -> String {
    let digest = TokenHash::of(&body.to_string());
    format!("\"{}\"", &digest.as_str()[..24])
}
