//! The REST API. Every call carries a bearer token that a team's token list
//! maps to an agent, a person or an ingest worker; the name on each record
//! comes from there, never from the body.
//!
//! The team a call acts in comes from the token. A path under
//! `/v1/teams/{team}/` names it too, and must name one of the token's teams.
//! Every handler gets its team and caller from [`Call`], so no handler can
//! reach a tracker the token was not given.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use kbl_core::content::Content;
use kbl_core::names::{AgentName, ItemId, Key};
use kbl_core::work::{
    CloseWhy, DoneWhen, Evidence, FindingVerdict, MergeVerdict, Phase, RevertCheck, Severity,
    Target, Unclear, WorkRecord,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::OwnedSemaphorePermit;
use tower::ServiceExt;

use crate::config::Caller;
use crate::felix::Broker;
use crate::teams::Teams;
use crate::tracker::{Decide, Error, NewFinding, Post, Prereq, Reply, Tracker, TriageAnswer};

mod ingest;
mod org;

/// A call the token does not allow: status and why.
type Denied = (StatusCode, &'static str);

struct App<B> {
    teams: Arc<Teams<B>>,
}

/// The team named by a `/v1/teams/{team}/` path, set by [`in_team`] only.
#[derive(Clone)]
struct TeamPath(String);

pub(crate) fn router<B: Broker>(teams: Arc<Teams<B>>) -> Router {
    let app = Arc::new(App { teams });
    let work = work_routes::<B>().with_state(app.clone());
    let scoped = work.clone();
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/me", get(me::<B>))
        .route(
            "/v1/org/teams",
            get(org::list_teams::<B>).post(org::create_team::<B>),
        )
        .route(
            "/v1/teams/{team}/{*rest}",
            any(move |req: Request| in_team(scoped.clone(), req)),
        )
        .with_state(app)
        .merge(work)
}

/// The routes that act in one team. They are served at `/v1/...`, for the
/// token's only team, and at `/v1/teams/{team}/...`.
fn work_routes<B: Broker>() -> Router<Arc<App<B>>> {
    Router::new()
        .route("/v1/work/next", post(next::<B>))
        .route("/v1/work/extend", post(extend::<B>))
        .route("/v1/work/complete", post(complete::<B>))
        .route("/v1/work/release", post(release::<B>))
        .route("/v1/work/give_up", post(give_up::<B>))
        .route("/v1/work/records", post(records::<B>))
        .merge(ingest::routes::<B>())
        .route("/v1/work/post", post(work_post::<B>))
        .route("/v1/work/link", post(link::<B>))
        .route("/v1/work/context", post(context_read::<B>))
        .route("/v1/work/context/add", post(context_add::<B>))
        .route("/v1/work/progress", post(progress::<B>))
        .route("/v1/work/decide", post(decide::<B>))
        .route("/v1/work/finding", post(finding::<B>))
        .route("/v1/work/verdict", post(review_verdict::<B>))
        .route("/v1/work/triage", post(triage::<B>))
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
        .route("/v1/work/rules", post(rules::<B>))
        .route("/v1/work/rules/read", post(rules_read::<B>))
}

/// Serves `/v1/teams/{team}/<rest>` as `/v1/<rest>` in that team. Only the
/// work and ingest routes have a team form.
async fn in_team(work: Router, mut req: Request) -> Response {
    // The raw path, so a percent-encoded segment reaches the inner route as sent.
    let path = req.uri().path().to_owned();
    let Some((team, rest)) = path
        .strip_prefix("/v1/teams/")
        .and_then(|p| p.split_once('/'))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !(rest.starts_with("work/") || rest.starts_with("ingest/")) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let target = match req.uri().query() {
        Some(q) => format!("/v1/{rest}?{q}"),
        None => format!("/v1/{rest}"),
    };
    let Ok(uri) = target.parse::<Uri>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    req.extensions_mut().insert(TeamPath(team.to_owned()));
    *req.uri_mut() = uri;
    match work.oneshot(req).await {
        Ok(res) => res,
        Err(never) => match never {},
    }
}

/// Who is calling and in which team, with the team's slot for the call. Taken
/// before the body is read, so an unknown token is refused first.
struct Call<B> {
    tracker: Tracker<B>,
    caller: Caller,
    _slot: OwnedSemaphorePermit,
}

impl<B: Broker> FromRequestParts<Arc<App<B>>> for Call<B> {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App<B>>) -> Result<Self, Response> {
        let mut found = bearer(&parts.headers)
            .map(|t| app.teams.who(t))
            .unwrap_or_default();
        if found.is_empty() {
            return Err(denied((
                StatusCode::UNAUTHORIZED,
                "unknown or missing token",
            )));
        }
        let (team, caller) = match parts.extensions.get::<TeamPath>() {
            // A team the token is not in answers as one that does not exist.
            Some(TeamPath(named)) => found
                .into_iter()
                .find(|(t, _)| t.name().as_str() == named)
                .ok_or_else(|| refused(StatusCode::NOT_FOUND, "no such team", "unknown_team"))?,
            None if found.len() > 1 => {
                return Err(refused(
                    StatusCode::BAD_REQUEST,
                    "this token is in more than one team; name the team in the path",
                    "team_required",
                ));
            }
            None => found.remove(0),
        };
        let Some(tracker) = team.tracker() else {
            let mut res = refused(
                StatusCode::SERVICE_UNAVAILABLE,
                "the team is not serving; it is being started again",
                "team_unavailable",
            );
            res.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
            return Err(res);
        };
        let Some(slot) = team.admit(parts.uri.path() == "/v1/work/next") else {
            let mut res = refused(
                StatusCode::SERVICE_UNAVAILABLE,
                "the team has too many calls in flight",
                "team_busy",
            );
            res.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            return Err(res);
        };
        Ok(Self {
            tracker,
            caller,
            _slot: slot,
        })
    }
}

impl<B> Call<B> {
    fn tracker(&self) -> &Tracker<B> {
        &self.tracker
    }

    fn agent(&self) -> Result<AgentName, Denied> {
        match &self.caller {
            Caller::Agent(a) => Ok(a.clone()),
            Caller::Person(_) | Caller::Ingest(_) => {
                Err((StatusCode::FORBIDDEN, "this call is for an agent's token"))
            }
        }
    }

    fn person(&self) -> Result<Key, Denied> {
        match &self.caller {
            Caller::Person(p) => Ok(p.clone()),
            Caller::Agent(_) | Caller::Ingest(_) => {
                Err((StatusCode::FORBIDDEN, "this call is for a person's token"))
            }
        }
    }
}

/// The person's teams and their role in each. A person in the token file is
/// an org admin, so is an admin in every team that file belongs to.
async fn me<B: Broker>(State(app): State<Arc<App<B>>>, headers: HeaderMap) -> Response {
    let found = bearer(&headers)
        .map(|t| app.teams.who(t))
        .unwrap_or_default();
    let Some((_, first)) = found.first() else {
        return denied((StatusCode::UNAUTHORIZED, "unknown or missing token"));
    };
    let Caller::Person(person) = first else {
        return denied((StatusCode::FORBIDDEN, "this call is for a person's token"));
    };
    let teams: Vec<Value> = found
        .iter()
        .map(|(t, _)| json!({ "team": t.name(), "role": "admin" }))
        .collect();
    Json(json!({ "person": person, "org_admin": true, "teams": teams })).into_response()
}

#[derive(Deserialize)]
struct NextBody {
    wait_seconds: Option<u64>,
    /// Reviewer tiers the agent takes reviews of.
    #[serde(default)]
    review_tiers: Vec<String>,
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
    #[serde(default)]
    title: String,
    /// For `prerequisite`: where the new work belongs.
    target_source: Option<String>,
    target_repo: Option<String>,
    /// For `prerequisite`: why the held item cannot go on without it.
    why: Option<Content>,
    #[serde(default)]
    evidence: Vec<Evidence>,
    /// For `prerequisite`: an item that exists already, instead of a new one.
    existing: Option<ItemId>,
    /// For `prerequisite`: `merged` (default) or `released`.
    #[serde(default)]
    done_when: DoneWhen,
    /// What the attempt cost so far.
    #[serde(default)]
    cost_micros: u64,
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
    #[serde(default)]
    revert_check: Option<RevertCheck>,
}

#[derive(Deserialize)]
struct FindingBody {
    item: ItemId,
    severity: Severity,
    file: String,
    line: u32,
    title: String,
    scenario: String,
    verdict: FindingVerdict,
}

#[derive(Deserialize)]
struct VerdictBody {
    item: ItemId,
    verdict: MergeVerdict,
    why: String,
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
    /// Free text, kept as written. Empty when `chosen` names an option.
    #[serde(default = "no_text")]
    text: Content,
    /// One of the question's options.
    chosen: Option<String>,
    /// The work belongs in another repo. Only a person may say so.
    moves_to: Option<MovesTo>,
    #[serde(default)]
    evidence: Vec<Evidence>,
}

#[derive(Deserialize)]
struct MovesTo {
    source: String,
    repo: String,
    title: String,
}

fn no_text() -> Content {
    Content::Text(String::new())
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

#[derive(Deserialize)]
struct RulesBody {
    source: String,
    repo: String,
    root: Option<ItemId>,
    text: Option<String>,
    checks: Option<Vec<String>>,
}

async fn next<B: Broker>(call: Call<B>, Json(body): Json<NextBody>) -> Response {
    let agent = match call.agent() {
        Ok(a) => a,
        Err(d) => return denied(d),
    };
    let wait = body
        .wait_seconds
        .map_or(Duration::from_secs(10), Duration::from_secs);
    reply(
        call.tracker()
            .next_for(&agent, wait, &body.review_tiers)
            .await,
    )
}

async fn extend<B: Broker>(call: Call<B>, Json(body): Json<ItemBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(call.tracker().extend(&a, &body.item).await),
        Err(d) => denied(d),
    }
}

async fn complete<B: Broker>(call: Call<B>, Json(body): Json<CompleteBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.tracker()
                .complete(&a, &body.item, body.cost_micros)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn release<B: Broker>(call: Call<B>, Json(body): Json<ReleaseBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.tracker()
                .release(&a, &body.item, body.reason, body.cost_micros)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn give_up<B: Broker>(call: Call<B>, Json(body): Json<GiveUpBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.tracker()
                .give_up(&a, &body.item, body.reason, body.cost_micros)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn records<B: Broker>(call: Call<B>, Json(body): Json<RecordBody>) -> Response {
    match call.person() {
        Ok(p) => reply(call.tracker().person_record(p, body.record).await),
        Err(d) => denied(d),
    }
}

async fn work_post<B: Broker>(call: Call<B>, Json(b): Json<PostBody>) -> Response {
    let a = match call.agent() {
        Ok(a) => a,
        Err(d) => return denied(d),
    };
    if b.work == "prerequisite" {
        let which = match (b.existing, b.target_source, b.target_repo, b.why) {
            (Some(id), None, None, None) if b.title.is_empty() => Prereq::Existing(id),
            (None, Some(source), Some(repo), Some(why)) => Prereq::New {
                target: Target { source, repo },
                title: b.title,
                why,
                evidence: b.evidence,
            },
            _ => {
                return reply(Err(Error::Invalid(
                    "a prerequisite names either `existing`, or a target_source, target_repo, title and why".into(),
                )));
            }
        };
        return reply(
            call.tracker()
                .prerequisite(&a, &b.item, b.key, which, b.done_when, b.cost_micros)
                .await,
        );
    }
    let kind = match Post::parse(&b.work) {
        Ok(k) => k,
        Err(e) => return reply(Err(e)),
    };
    reply(call.tracker().post(&a, &b.item, b.key, kind, b.title).await)
}

async fn link<B: Broker>(call: Call<B>, Json(b): Json<LinkBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(call.tracker().link(&a, &b.item, &b.to).await),
        Err(d) => denied(d),
    }
}

async fn context_read<B: Broker>(call: Call<B>, Json(b): Json<ItemBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(call.tracker().context_read(&a, &b.item).await),
        Err(d) => denied(d),
    }
}

async fn context_add<B: Broker>(call: Call<B>, Json(b): Json<ContextAddBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.tracker()
                .context_add(&a, &b.item, b.entry, b.text)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn progress<B: Broker>(call: Call<B>, Json(b): Json<ProgressBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.tracker()
                .progress(&a, &b.item, b.note, b.branch, b.pr)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn decide<B: Broker>(call: Call<B>, Json(b): Json<DecideBody>) -> Response {
    let a = match call.agent() {
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
        revert_check: b.revert_check,
    };
    reply(call.tracker().decide(&a, &b.item, d).await)
}

async fn finding<B: Broker>(call: Call<B>, Json(b): Json<FindingBody>) -> Response {
    let a = match call.agent() {
        Ok(a) => a,
        Err(d) => return denied(d),
    };
    let f = NewFinding {
        severity: b.severity,
        file: b.file,
        line: b.line,
        title: b.title,
        scenario: b.scenario,
        verdict: b.verdict,
    };
    reply(call.tracker().finding(&a, &b.item, f).await)
}

async fn review_verdict<B: Broker>(call: Call<B>, Json(b): Json<VerdictBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.tracker()
                .review_verdict(&a, &b.item, b.verdict, b.why)
                .await,
        ),
        Err(d) => denied(d),
    }
}

#[derive(Deserialize)]
struct TriageBody {
    item: ItemId,
    tier: u8,
    why: String,
    #[serde(default)]
    duplicate_of: Option<ItemId>,
    #[serde(default)]
    unclear: Option<Unclear>,
    #[serde(default)]
    depends_on: Vec<ItemId>,
    #[serde(default)]
    split: Vec<String>,
}

async fn triage<B: Broker>(call: Call<B>, Json(b): Json<TriageBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.tracker()
                .triage(
                    &a,
                    &b.item,
                    TriageAnswer {
                        tier: b.tier,
                        why: b.why,
                        duplicate_of: b.duplicate_of,
                        unclear: b.unclear,
                        depends_on: b.depends_on,
                        split: b.split,
                    },
                )
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn step<B: Broker>(call: Call<B>, Json(b): Json<StepBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.tracker()
                .record_step(&a, &b.item, b.command, b.exit_code, b.output)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn edit<B: Broker>(call: Call<B>, Json(b): Json<EditBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.tracker()
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
    #[serde(default)]
    options: Vec<String>,
    default: Option<String>,
    /// What the attempt cost so far. Asking ends the hold, so this is the last chance to report it.
    #[serde(default)]
    cost_micros: u64,
}

async fn ask<B: Broker>(call: Call<B>, Json(b): Json<AskBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.tracker()
                .ask(&a, &b.item, b.text, b.options, b.default, b.cost_micros)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn answer<B: Broker>(call: Call<B>, Json(b): Json<AnswerBody>) -> Response {
    let a = match call.agent() {
        Ok(a) => a,
        Err(d) => return denied(d),
    };
    if b.moves_to.is_some() {
        return denied((
            StatusCode::FORBIDDEN,
            "only a person can move work to another repo",
        ));
    }
    let Some(question) = b.question else {
        return denied((
            StatusCode::UNPROCESSABLE_ENTITY,
            "name the question's offset",
        ));
    };
    reply(
        call.tracker()
            .answer(&a, &b.item, question, b.text, b.evidence, b.chosen)
            .await,
    )
}

/// A person's action that is one record.
async fn as_person<B: Broker>(call: &Call<B>, record: WorkRecord) -> Response {
    match call.person() {
        Ok(p) => reply(call.tracker().person_record(p, record).await),
        Err(d) => denied(d),
    }
}

async fn accept<B: Broker>(call: Call<B>, Json(b): Json<AcceptBody>) -> Response {
    let record = WorkRecord::Accepted {
        item: b.item,
        tier: b.tier,
    };
    as_person(&call, record).await
}

async fn edit_proposal<B: Broker>(call: Call<B>, Json(b): Json<EditProposalBody>) -> Response {
    let record = WorkRecord::Edited {
        item: b.item,
        title: b.title,
        tier: b.tier,
    };
    as_person(&call, record).await
}

async fn reject<B: Broker>(call: Call<B>, Json(b): Json<ItemBody>) -> Response {
    as_person(&call, WorkRecord::Rejected { item: b.item }).await
}

async fn person_answer<B: Broker>(call: Call<B>, Json(b): Json<AnswerBody>) -> Response {
    let how = Reply {
        chosen: b.chosen,
        moves_to: b.moves_to.map(|m| {
            (
                Target {
                    source: m.source,
                    repo: m.repo,
                },
                m.title,
            )
        }),
    };
    match call.person() {
        Ok(p) => reply(
            call.tracker()
                .person_answer(p, &b.item, b.question, b.text, b.evidence, how)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn tier<B: Broker>(call: Call<B>, Json(b): Json<TierBody>) -> Response {
    let record = WorkRecord::TierChanged {
        item: b.item,
        tier: b.tier,
    };
    as_person(&call, record).await
}

async fn close<B: Broker>(call: Call<B>, Json(b): Json<CloseBody>) -> Response {
    let record = WorkRecord::Closed {
        item: b.item,
        why: b.why,
    };
    as_person(&call, record).await
}

async fn unlink<B: Broker>(call: Call<B>, Json(b): Json<LinkBody>) -> Response {
    let record = WorkRecord::Unlinked {
        item: b.item,
        to: b.to,
    };
    as_person(&call, record).await
}

async fn budget<B: Broker>(call: Call<B>, Json(b): Json<BudgetBody>) -> Response {
    let record = WorkRecord::BudgetSet {
        root: b.root,
        items_left: b.items_left,
        max_depth: b.max_depth,
        cost_left_micros: b.cost_left_micros,
        redos_per_item: b.redos_per_item,
    };
    as_person(&call, record).await
}

async fn rules<B: Broker>(call: Call<B>, Json(b): Json<RulesBody>) -> Response {
    let record = WorkRecord::RulesSet {
        target: Target {
            source: b.source,
            repo: b.repo,
        },
        root: b.root,
        text: b.text,
        checks: b.checks,
    };
    as_person(&call, record).await
}

async fn rules_read<B: Broker>(call: Call<B>, Json(b): Json<ItemBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(call.tracker().rules_read(&a, &b.item).await),
        Err(d) => denied(d),
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
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

fn refused(status: StatusCode, message: &str, why: &str) -> Response {
    (status, Json(json!({ "error": message, "why": why }))).into_response()
}

#[cfg(test)]
mod tests;
