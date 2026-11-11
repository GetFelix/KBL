//! The calls only the ingest worker makes: the upstream sync records, the
//! roots it imported, and its saved state. Its token cannot claim work or
//! write the records a person writes.

use std::sync::Arc;

use agents_core::names::Key;
use agents_core::work::WorkRecord;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use super::{App, Caller, StatusCode, caller, denied, reply};
use crate::felix::Broker;

pub(super) fn routes<B: Broker>() -> Router<Arc<App<B>>> {
    Router::new()
        .route("/v1/ingest/records", post(record::<B>))
        .route("/v1/ingest/roots", get(roots::<B>))
        .route("/v1/ingest/state/{source}", get(load::<B>).put(save::<B>))
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

fn worker<B>(app: &App<B>, headers: &HeaderMap) -> Result<Key, super::Denied> {
    match caller(app, headers)? {
        Caller::Ingest(k) => Ok(k),
        _ => Err((StatusCode::FORBIDDEN, "this call is for an ingest token")),
    }
}

async fn record<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(body): Json<RecordBody>,
) -> Response {
    match worker(&app, &headers) {
        Ok(by) => reply(app.tracker.ingest_record(by, body.record).await),
        Err(d) => denied(d),
    }
}

async fn roots<B: Broker>(State(app): State<Arc<App<B>>>, headers: HeaderMap) -> Response {
    match worker(&app, &headers) {
        Ok(_) => reply(app.tracker.ingest_roots().await),
        Err(d) => denied(d),
    }
}

async fn load<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Path(source): Path<String>,
) -> Response {
    match worker(&app, &headers) {
        Ok(_) => reply(app.tracker.ingest_state(&source).await),
        Err(d) => denied(d),
    }
}

async fn save<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Path(source): Path<String>,
    Json(body): Json<SaveBody>,
) -> Response {
    match worker(&app, &headers) {
        Ok(_) => reply(
            app.tracker
                .ingest_save(&source, &body.state, body.version)
                .await,
        ),
        Err(d) => denied(d),
    }
}
