//! The calls only the ingest worker makes: the upstream sync records, the
//! roots it imported, and its saved state. Its token cannot claim work or
//! write the records a person writes.

use std::sync::Arc;

use axum::extract::Path;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use kbl_core::work::WorkRecord;
use serde::Deserialize;

use super::{App, Call, Caller, StatusCode, denied, reply};
use crate::config::Worker;
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
        Ok(_) => reply(call.tracker().ingest_roots().await),
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
