//! The REST API. Every call carries a bearer token that the token file maps to
//! an agent or a person; the name on each record comes from there, never from
//! the body.

use std::sync::Arc;
use std::time::Duration;

use agents_core::content::Content;
use agents_core::names::{AgentName, ItemId, Key};
use agents_core::work::{CloseWhy, Evidence, Phase, WorkRecord};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{Caller, Tokens};
use crate::felix::Broker;
use crate::tracker::{Decide, Error, Post, Tracker};

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
        .route("/v1/work/post", post(work_post::<B>))
        .route("/v1/work/link", post(link::<B>))
        .route("/v1/work/context", post(context_read::<B>))
        .route("/v1/work/context/add", post(context_add::<B>))
        .route("/v1/work/progress", post(progress::<B>))
        .route("/v1/work/decide", post(decide::<B>))
        .route("/v1/work/step", post(step::<B>))
        .route("/v1/work/edit", post(edit::<B>))
        .route("/v1/work/ask", post(ask::<B>))
        .route("/v1/work/answer", post(answer::<B>))
        .route("/v1/work/proposals/accept", post(accept::<B>))
        .route("/v1/work/proposals/edit", post(edit_proposal::<B>))
        .route("/v1/work/proposals/reject", post(reject::<B>))
        .route("/v1/work/questions/answer", post(person_answer::<B>))
        .route("/v1/work/tier", post(tier::<B>))
        .route("/v1/work/close", post(close::<B>))
        .route("/v1/work/unlink", post(unlink::<B>))
        .route("/v1/work/budget", post(budget::<B>))
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

#[derive(Deserialize)]
struct PostBody {
    /// The item the agent holds.
    item: ItemId,
    key: Key,
    work: String,
    title: String,
}

#[derive(Deserialize)]
struct LinkBody {
    item: ItemId,
    to: ItemId,
}

#[derive(Deserialize)]
struct ContextAddBody {
    item: ItemId,
    entry: Key,
    text: Content,
}

#[derive(Deserialize)]
struct ProgressBody {
    item: ItemId,
    note: Content,
    branch: Option<String>,
    pr: Option<String>,
}

#[derive(Deserialize)]
struct DecideBody {
    item: ItemId,
    phase: Phase,
    so_far: String,
    #[serde(default)]
    question: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    chosen: String,
    why: String,
    commit: String,
    #[serde(default)]
    evidence: Vec<Evidence>,
}

#[derive(Deserialize)]
struct StepBody {
    item: ItemId,
    command: String,
    exit_code: i32,
    output: Content,
}

#[derive(Deserialize)]
struct EditBody {
    item: ItemId,
    file: String,
    hunk: Content,
    decision: u64,
}

#[derive(Deserialize)]
struct AnswerBody {
    item: ItemId,
    /// The offset of the question. A person's answer defaults to the open one.
    question: Option<u64>,
    text: Content,
    #[serde(default)]
    evidence: Vec<Evidence>,
}

#[derive(Deserialize)]
struct AcceptBody {
    item: ItemId,
    tier: Option<u8>,
}

#[derive(Deserialize)]
struct EditProposalBody {
    item: ItemId,
    title: Option<String>,
    tier: Option<u8>,
}

#[derive(Deserialize)]
struct TierBody {
    item: ItemId,
    tier: u8,
}

#[derive(Deserialize)]
struct CloseBody {
    item: ItemId,
    #[serde(default = "dismissed")]
    why: CloseWhy,
}

fn dismissed() -> CloseWhy {
    CloseWhy::Dismissed
}

#[derive(Deserialize)]
struct BudgetBody {
    root: ItemId,
    items_left: u32,
    max_depth: u8,
    cost_left_micros: u64,
    redos_per_item: u8,
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

async fn work_post<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<PostBody>,
) -> Response {
    let a = match agent(&app, &headers) {
        Ok(a) => a,
        Err(d) => return denied(d),
    };
    let kind = match Post::parse(&b.work) {
        Ok(k) => k,
        Err(e) => return reply(Err(e)),
    };
    reply(app.tracker.post(&a, &b.item, b.key, kind, b.title).await)
}

async fn link<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<LinkBody>,
) -> Response {
    match agent(&app, &headers) {
        Ok(a) => reply(app.tracker.link(&a, &b.item, &b.to).await),
        Err(d) => denied(d),
    }
}

async fn context_read<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<ItemBody>,
) -> Response {
    match agent(&app, &headers) {
        Ok(_) => reply(app.tracker.context_read(&b.item).await),
        Err(d) => denied(d),
    }
}

async fn context_add<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<ContextAddBody>,
) -> Response {
    match agent(&app, &headers) {
        Ok(a) => reply(app.tracker.context_add(&a, &b.item, b.entry, b.text).await),
        Err(d) => denied(d),
    }
}

async fn progress<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<ProgressBody>,
) -> Response {
    match agent(&app, &headers) {
        Ok(a) => reply(
            app.tracker
                .progress(&a, &b.item, b.note, b.branch, b.pr)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn decide<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<DecideBody>,
) -> Response {
    let a = match agent(&app, &headers) {
        Ok(a) => a,
        Err(d) => return denied(d),
    };
    let d = Decide {
        phase: b.phase,
        so_far: b.so_far,
        question: b.question,
        options: b.options,
        chosen: b.chosen,
        why: b.why,
        commit: b.commit,
        evidence: b.evidence,
    };
    reply(app.tracker.decide(&a, &b.item, d).await)
}

async fn step<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<StepBody>,
) -> Response {
    match agent(&app, &headers) {
        Ok(a) => reply(
            app.tracker
                .record_step(&a, &b.item, b.command, b.exit_code, b.output)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn edit<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<EditBody>,
) -> Response {
    match agent(&app, &headers) {
        Ok(a) => reply(
            app.tracker
                .edit(&a, &b.item, b.file, b.hunk, b.decision)
                .await,
        ),
        Err(d) => denied(d),
    }
}

#[derive(Deserialize)]
struct AskBody {
    item: ItemId,
    text: Content,
}

async fn ask<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<AskBody>,
) -> Response {
    match agent(&app, &headers) {
        Ok(a) => reply(app.tracker.ask(&a, &b.item, b.text).await),
        Err(d) => denied(d),
    }
}

async fn answer<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<AnswerBody>,
) -> Response {
    let a = match agent(&app, &headers) {
        Ok(a) => a,
        Err(d) => return denied(d),
    };
    let Some(question) = b.question else {
        return denied((
            StatusCode::UNPROCESSABLE_ENTITY,
            "name the question's offset",
        ));
    };
    reply(
        app.tracker
            .answer(&a, &b.item, question, b.text, b.evidence)
            .await,
    )
}

/// A person's action that is one record.
async fn as_person<B: Broker>(app: &App<B>, headers: &HeaderMap, record: WorkRecord) -> Response {
    match person(app, headers) {
        Ok(p) => reply(app.tracker.person_record(p, record).await),
        Err(d) => denied(d),
    }
}

async fn accept<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<AcceptBody>,
) -> Response {
    let record = WorkRecord::Accepted {
        item: b.item,
        tier: b.tier,
    };
    as_person(&app, &headers, record).await
}

async fn edit_proposal<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<EditProposalBody>,
) -> Response {
    let record = WorkRecord::Edited {
        item: b.item,
        title: b.title,
        tier: b.tier,
    };
    as_person(&app, &headers, record).await
}

async fn reject<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<ItemBody>,
) -> Response {
    as_person(&app, &headers, WorkRecord::Rejected { item: b.item }).await
}

async fn person_answer<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<AnswerBody>,
) -> Response {
    match person(&app, &headers) {
        Ok(p) => reply(
            app.tracker
                .person_answer(p, &b.item, b.question, b.text, b.evidence)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn tier<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<TierBody>,
) -> Response {
    let record = WorkRecord::TierChanged {
        item: b.item,
        tier: b.tier,
    };
    as_person(&app, &headers, record).await
}

async fn close<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<CloseBody>,
) -> Response {
    let record = WorkRecord::Closed {
        item: b.item,
        why: b.why,
    };
    as_person(&app, &headers, record).await
}

async fn unlink<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<LinkBody>,
) -> Response {
    let record = WorkRecord::Unlinked {
        item: b.item,
        to: b.to,
    };
    as_person(&app, &headers, record).await
}

async fn budget<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    Json(b): Json<BudgetBody>,
) -> Response {
    let record = WorkRecord::BudgetSet {
        root: b.root,
        items_left: b.items_left,
        max_depth: b.max_depth,
        cost_left_micros: b.cost_left_micros,
        redos_per_item: b.redos_per_item,
    };
    as_person(&app, &headers, record).await
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
        Caller::Person(_) => Err((StatusCode::FORBIDDEN, "this call is for an agent's token")),
    }
}

fn person<B>(app: &App<B>, headers: &HeaderMap) -> Result<Key, Denied> {
    match caller(app, headers)? {
        Caller::Person(p) => Ok(p),
        Caller::Agent(_) => Err((StatusCode::FORBIDDEN, "this call is for a person's token")),
    }
}

fn reply(result: Result<Value, Error>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => {
            let (status, extra) = match &e {
                Error::Refused { why } => (StatusCode::CONFLICT, json!({ "why": why })),
                Error::Invalid(_) => (StatusCode::BAD_REQUEST, json!({ "why": "invalid" })),
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
