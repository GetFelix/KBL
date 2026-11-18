//! The REST API. Every call carries a bearer token that a team's token list
//! maps to an agent, a person or an ingest worker; the name on each record
//! comes from there, never from the body.
//!
//! The team a call acts in comes from the token. A path under
//! `/v1/teams/{team}/` names it too, and must name one of the token's teams.
//! Every handler gets its team and caller from [`Call`], so no handler can
//! reach a tracker the token was not given.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{FromRequest, FromRequestParts, MatchedPath, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use kbl_core::content::Content;
use kbl_core::names::{AgentName, ItemId, Key};
use kbl_core::work::{
    Actor, Assignee, CloseWhy, DoneWhen, Evidence, FindingVerdict, HumanKind, MergeVerdict, Phase,
    PriceUnit, ReleaseWhy, RequestKey, RevertCheck, Severity, Target, Unclear, Usage, WorkRecord,
};
use kbl_core::work_fold::Stage;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::OwnedSemaphorePermit;
use tower::ServiceExt;
use tracing::Instrument as _;

use crate::config::Caller;
use crate::felix::Broker;
use crate::org::OrgUnavailable;
use crate::teams::{Team, Teams};
use crate::tracker::ExpectedPrice;
use crate::tracker::selection::Filter;
use crate::tracker::{
    Decide, Error, NewFinding, Post, Prereq, Reply, Tracker, TriageAnswer, Which,
};

mod agents;
pub(crate) mod ingest;
mod org;

/// A call the token does not allow: status and why.
type Denied = (StatusCode, &'static str);

struct App<B> {
    teams: Arc<Teams<B>>,
    /// Config long polls held at once, across teams. Past it a poll answers
    /// at once instead of waiting, so held calls cannot pile up unbounded.
    polls: Arc<tokio::sync::Semaphore>,
    /// Held polls per team, so one team cannot take every held slot.
    team_polls: std::sync::Mutex<
        std::collections::HashMap<kbl_core::org::TeamName, Arc<tokio::sync::Semaphore>>,
    >,
    /// Live ingest workers per group and the source leases they hold.
    leases: Arc<ingest::leases::Groups>,
}

/// The team named by a `/v1/teams/{team}/` path, set by [`in_team`] only.
#[derive(Clone)]
struct TeamPath(String);

pub(crate) fn router<B: Broker>(teams: Arc<Teams<B>>) -> Router {
    router_with(teams, Default::default())
}

pub(crate) fn router_with<B: Broker>(
    teams: Arc<Teams<B>>,
    leases: Arc<ingest::leases::Groups>,
) -> Router {
    let app = Arc::new(App {
        teams,
        polls: Arc::new(tokio::sync::Semaphore::new(ingest::MAX_HELD_POLLS)),
        team_polls: Default::default(),
        leases,
    });
    // `route_layer` sees the matched route template, so the metrics label is
    // `/v1/work/items/{item}`, never an item id. The team-scoped catch-all is added
    // after the layer: the inner router's routes are counted once, not twice.
    let work = work_routes::<B>()
        .route_layer(from_fn_with_state(app.clone(), observe::<B>))
        .with_state(app.clone());
    let scoped = work.clone();
    let for_agents = app.clone();
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/me", get(me::<B>))
        .route(
            "/v1/org/teams",
            get(org::list_teams::<B>).post(org::create_team::<B>),
        )
        .route("/v1/org/records", post(org::org_record::<B>))
        .route_layer(from_fn_with_state(app.clone(), observe::<B>))
        .route(
            "/v1/teams/{team}/{*rest}",
            any(move |req: Request| in_team(scoped.clone(), for_agents.clone(), req)),
        )
        .with_state(app)
        .merge(work)
}

/// The unauthenticated landing answer: what this is and where its health check
/// is. It carries no configuration, token or state.
async fn index() -> Json<Value> {
    Json(json!({
        "service": "kbl-tracker",
        "version": env!("CARGO_PKG_VERSION"),
        "health": "/healthz",
    }))
}

/// Counts the call and times it, by route template, method and status.
async fn observe<B: Broker>(State(app): State<Arc<App<B>>>, req: Request, next: Next) -> Response {
    // The layer only runs on matched routes, so there is always a template.
    let Some(route) = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned())
    else {
        return next.run(req).await;
    };
    let method = req.method().clone();
    // Named by the template, with the same fixed method set as the metrics.
    let span = tracing::info_span!(
        "http.request",
        otel.name = format!("{} {route}", method_label(method.as_str())),
        http.route = %route,
        http.request.method = method_label(method.as_str()),
        http.response.status_code = tracing::field::Empty,
    );
    // The span starts when it is first entered, so the parent has to be set
    // before that, not once the handler has checked the token. A caller with
    // no matching token cannot pick the trace or its sampling flag: its span
    // stays a root under the tracker's own sampler.
    if knows_token(&app, req.headers()) {
        kbl_trace::adopt_parent(&span, req.headers());
    }
    let started = std::time::Instant::now();
    let res = next.run(req).instrument(span.clone()).await;
    span.record("http.response.status_code", res.status().as_u16());
    record_request(&route, method.as_str(), res.status(), started.elapsed());
    res
}

/// Whether the request's bearer token is one the tracker accepts, for any call.
fn knows_token<B: Broker>(app: &App<B>, headers: &HeaderMap) -> bool {
    let Some(token) = bearer(headers) else {
        return false;
    };
    app.teams.org_admin(token).is_some() || matches!(app.teams.who(token), Ok(f) if !f.is_empty())
}

/// The method label is one of a fixed set, so a client cannot mint labels.
fn method_label(method: &str) -> &'static str {
    match method {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "DELETE" => "DELETE",
        "PATCH" => "PATCH",
        _ => "other",
    }
}

fn record_request(route: &str, method: &str, status: StatusCode, took: Duration) {
    let method = method_label(method).to_owned();
    let (route, status) = (route.to_owned(), status.as_u16().to_string());
    metrics::counter!(
        "kbl_tracker_http_requests_total",
        "route" => route.clone(), "method" => method.clone(), "status" => status.clone()
    )
    .increment(1);
    metrics::histogram!(
        "kbl_tracker_http_request_seconds",
        "route" => route, "method" => method, "status" => status
    )
    .record(took.as_secs_f64());
}

/// The routes that act in one team. They are served at `/v1/...`, for the
/// token's only team, and at `/v1/teams/{team}/...`.
fn work_routes<B: Broker>() -> Router<Arc<App<B>>> {
    Router::new()
        .route("/v1/work/next", post(next::<B>))
        .route("/v1/work/peek", post(peek::<B>))
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
        .route("/v1/work/note", post(note::<B>))
        .route("/v1/work/tier", post(tier::<B>))
        .route("/v1/work/close", post(close::<B>))
        .route("/v1/work/unlink", post(unlink::<B>))
        .route("/v1/work/budget", post(budget::<B>))
        .route("/v1/work/prices", post(price::<B>))
        .route("/v1/work/rules", post(rules::<B>))
        .route("/v1/work/rules/read", post(rules_read::<B>))
        .route("/v1/work/flag", post(flag::<B>))
        .route("/v1/work/unflag", post(unflag::<B>))
        .route("/v1/work/retry", post(retry::<B>))
        .route("/v1/work/snooze", post(snooze::<B>))
        .route("/v1/work/park", post(park::<B>))
        .route("/v1/work/unpark", post(unpark::<B>))
        .route("/v1/work/unblock", post(unblock::<B>))
        .route("/v1/work/request-review", post(request_review::<B>))
        .route("/v1/work/request-changes", post(request_changes::<B>))
        .route("/v1/work/mark-merged", post(mark_merged::<B>))
        .route("/v1/work/rate-summary", post(rate_summary::<B>))
        .route("/v1/work/take", post(take::<B>))
        .route("/v1/work/reassign", post(reassign::<B>))
        .route("/v1/work/steer", post(steer::<B>))
        .route("/v1/work/queue", get(queue::<B>))
        .route("/v1/work/items", get(items::<B>))
        .route("/v1/work/agents", get(team_agents::<B>))
        .route("/v1/work/items/{item}", get(item::<B>))
        .route("/v1/work/items/{item}/history", get(history::<B>))
        .route("/v1/work/metrics/time-to-human", get(time_to_human::<B>))
        .route("/v1/work/metrics/summary-trust", get(summary_trust::<B>))
}

/// Serves `/v1/teams/{team}/<rest>` as `/v1/<rest>` in that team. Only the
/// work and ingest routes have a team form; the agent routes exist only here.
async fn in_team<B: Broker>(work: Router, app: Arc<App<B>>, req: Request) -> Response {
    // The raw path, so a percent-encoded segment reaches the inner route as sent.
    let path = req.uri().path().to_owned();
    let Some((team, rest)) = path
        .strip_prefix("/v1/teams/")
        .and_then(|p| p.split_once('/'))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if rest == "agents" || rest.starts_with("agents/") {
        let (team, rest) = (team.to_owned(), rest.to_owned());
        let method = req.method().clone();
        let started = std::time::Instant::now();
        let res = agents::route(app, &team, &rest, req).await;
        record_request(
            "/v1/teams/{team}/agents/{*rest}",
            method.as_str(),
            res.status(),
            started.elapsed(),
        );
        return res;
    }
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
    // A fresh request: the outer route's `team` and `rest` params sit in the
    // extensions, and axum adds the inner route's to them, so a `Path` there
    // would see three.
    let team = TeamPath(team.to_owned());
    let (parts, body) = req.into_parts();
    let mut req = Request::new(body);
    *req.method_mut() = parts.method;
    *req.uri_mut() = uri;
    *req.version_mut() = parts.version;
    *req.headers_mut() = parts.headers;
    req.extensions_mut().insert(team);
    match work.oneshot(req).await {
        Ok(res) => res,
        Err(never) => match never {},
    }
}

/// A JSON body, and the request key it carries, if any. The digest covers the
/// route and the rest of the body, so a key sent again with another body can be
/// told from a retry.
struct Keyed<T>(T, Option<RequestKey>);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Keyed<T> {
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Response> {
        let route = req.uri().path().to_owned();
        let Json(mut body) = Json::<Value>::from_request(req, state)
            .await
            .map_err(IntoResponse::into_response)?;
        let key = match body.as_object_mut().and_then(|o| o.remove("request_key")) {
            None | Some(Value::Null) => None,
            Some(Value::String(k)) => Some(Key::new(k).map_err(|e| {
                reply(Err(Error::Invalid(format!(
                    "request_key: {e}; use letters, digits, - and _"
                ))))
            })?),
            Some(_) => {
                return Err(reply(Err(Error::Invalid(
                    "request_key must be a string".into(),
                ))));
            }
        };
        let req = key.map(|k| RequestKey::new(k, &route, &body));
        let typed = serde_json::from_value(body).map_err(|e| {
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("Failed to deserialize the JSON body into the target type: {e}"),
            )
                .into_response()
        })?;
        Ok(Self(typed, req))
    }
}

/// Who is calling and in which team, with the team's slot for the call. Taken
/// before the body is read, so an unknown token is refused first.
struct Call<B> {
    tracker: Tracker<B>,
    team: Arc<Team<B>>,
    teams: Arc<Teams<B>>,
    caller: Caller,
    _slot: OwnedSemaphorePermit,
}

impl<B: Broker> FromRequestParts<Arc<App<B>>> for Call<B> {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App<B>>) -> Result<Self, Response> {
        let mut found = match bearer(&parts.headers).map(|t| app.teams.who(t)) {
            Some(Ok(found)) => found,
            Some(Err(OrgUnavailable)) => return Err(org_unavailable()),
            None => Vec::new(),
        };
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
            team,
            teams: app.teams.clone(),
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

    /// Runs a write under the caller's request key, if it sent one.
    async fn keyed(
        &self,
        req: Option<RequestKey>,
        write: impl Future<Output = Result<Value, Error>>,
    ) -> Result<Value, Error>
    where
        B: Broker,
    {
        let (actor, by) = match &self.caller {
            Caller::Agent(a) => (
                Actor::Agent,
                Key::new(a.as_str()).expect("an agent name is a key"),
            ),
            Caller::Person(p) => (Actor::Person, p.clone()),
            Caller::Ingest(_) => return write.await,
        };
        self.tracker.keyed(actor, &by, req, write).await
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
    let found = match bearer(&headers).map(|t| app.teams.who(t)) {
        Some(Ok(found)) => found,
        Some(Err(OrgUnavailable)) => return org_unavailable(),
        None => Vec::new(),
    };
    let Some((_, first)) = found.first() else {
        return denied((StatusCode::UNAUTHORIZED, "unknown or missing token"));
    };
    let Caller::Person(person) = first else {
        return denied((StatusCode::FORBIDDEN, "this call is for a person's token"));
    };
    let teams: Vec<Value> = found
        .iter()
        .map(|(t, _)| {
            // Null while the team is not serving, like its other calls answer 503.
            let limit = t.tracker().map(|tr| tr.review_limit());
            json!({ "team": t.name(), "role": "admin", "review_limit": limit })
        })
        .collect();
    Json(json!({ "person": person, "org_admin": true, "teams": teams })).into_response()
}

#[derive(Deserialize)]
struct NextBody {
    wait_seconds: Option<u64>,
    /// Reviewer tiers the agent takes reviews of.
    #[serde(default)]
    review_tiers: Vec<String>,
    #[serde(flatten)]
    filter: FilterBody,
}

/// What a caller narrows `work_next` and `work_peek` to.
#[derive(Deserialize, Default)]
struct FilterBody {
    /// Kinds of work to take, such as `triage` or `fix`.
    #[serde(default)]
    kinds: Vec<String>,
    /// Priority tiers to take, 0 the highest.
    #[serde(default)]
    tiers: Vec<u8>,
    /// Only work that lands in this repo, as `owner/name`.
    repo: Option<String>,
    /// Roots whose work is not taken.
    #[serde(default)]
    exclude_roots: Vec<ItemId>,
}

impl FilterBody {
    fn parse(&self) -> Result<Filter, Error> {
        Filter::parse(
            &self.kinds,
            &self.tiers,
            self.repo.as_deref(),
            &self.exclude_roots,
        )
    }
}

#[derive(Deserialize)]
struct PeekBody {
    /// How many items to list; default 10.
    limit: Option<usize>,
    #[serde(default)]
    review_tiers: Vec<String>,
    #[serde(flatten)]
    filter: FilterBody,
}

#[derive(Deserialize)]
struct ItemBody {
    item: ItemId,
}

#[derive(Deserialize)]
struct SnoozeBody {
    item: ItemId,
    until_ms: u64,
}

#[derive(Deserialize)]
struct ReviewBody {
    item: ItemId,
    reviewer: String,
}

#[derive(Deserialize)]
struct ReassignBody {
    item: ItemId,
    to: Option<Assignee>,
}

#[derive(Deserialize)]
struct SteerBody {
    item: ItemId,
    attempt: u32,
    text: Content,
}

#[derive(Deserialize)]
struct RateBody {
    item: ItemId,
    attempt: u32,
    accurate: bool,
}

#[derive(Deserialize)]
struct UnblockBody {
    item: ItemId,
    to: ItemId,
}

#[derive(Deserialize)]
struct CompleteBody {
    item: ItemId,
    /// The attempt the claim was on, from the claim's reply. With it a retry
    /// after the record landed answers `replayed: true`.
    attempt: Option<u32>,
    /// The offset of the claim's `claimed` record, from the claim's reply. It
    /// names the claim exactly, and wins over `attempt`.
    claim: Option<u64>,
    /// What each model used during the attempt; the tracker prices it.
    #[serde(default)]
    usage: Vec<Usage>,
    /// Replaced by `usage`: refused, so an agent built for it learns why.
    cost_micros: Option<u64>,
}

#[derive(Deserialize)]
struct ReleaseBody {
    item: ItemId,
    /// The attempt the claim was on, from the claim's reply. With it a retry
    /// after the record landed answers `replayed: true`.
    attempt: Option<u32>,
    /// The offset of the claim's `claimed` record, from the claim's reply. It
    /// names the claim exactly, and wins over `attempt`.
    claim: Option<u64>,
    reason: Option<String>,
    /// `failed` (the default), `waits_on_person` or `waits_on_outside`.
    #[serde(default)]
    why: Option<ReleaseWhy>,
    /// What each model used during the attempt; the tracker prices it.
    #[serde(default)]
    usage: Vec<Usage>,
    /// Replaced by `usage`: refused, so an agent built for it learns why.
    cost_micros: Option<u64>,
}

#[derive(Deserialize)]
struct GiveUpBody {
    item: ItemId,
    /// The attempt the claim was on, from the claim's reply. With it a retry
    /// after the record landed answers `replayed: true`.
    attempt: Option<u32>,
    /// The offset of the claim's `claimed` record, from the claim's reply. It
    /// names the claim exactly, and wins over `attempt`.
    claim: Option<u64>,
    reason: String,
    /// What each model used during the attempt; the tracker prices it.
    #[serde(default)]
    usage: Vec<Usage>,
    /// Replaced by `usage`: refused, so an agent built for it learns why.
    cost_micros: Option<u64>,
}

fn claim_of(claim: Option<u64>, attempt: Option<u32>) -> Option<Which> {
    claim.map(Which::Claim).or(attempt.map(Which::Attempt))
}

/// The usage a claim end reports. A cost above 0 is refused rather than
/// dropped, so an agent still sending one is told what to send instead. An
/// older `kbl-mcp` always sends 0, which passes and is charged the floor.
fn usage_of(usage: Vec<Usage>, cost_micros: Option<u64>) -> Result<Vec<Usage>, Error> {
    match cost_micros {
        None | Some(0) => Ok(usage),
        Some(_) => Err(Error::Invalid(
            "cost_micros is no longer read: report `usage`, one entry per model, and the tracker prices it".into(),
        )),
    }
}

#[derive(Deserialize)]
struct PriceBody {
    model: String,
    unit: PriceUnit,
    price_micros: u64,
    effective_from_ms: Option<u64>,
    as_of: Option<String>,
    /// What the caller saw: the price this one replaces, or `{"none": true}`
    /// for no price in effect. Refused as `price_changed` when it is not what
    /// is in effect. Absent means no check.
    expected_price: Option<ExpectedPriceBody>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ExpectedPriceBody {
    Seen(SeenPrice),
    None(#[allow(dead_code)] NoPrice),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SeenPrice {
    price_micros: u64,
    /// When that price took effect, in milliseconds.
    since_ms: u64,
}

/// `{"none": true}`. A named field, and only `true` is accepted, so no price
/// read wrongly or left out turns into "expect none".
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoPrice {
    #[serde(deserialize_with = "only_true")]
    #[allow(dead_code)]
    none: bool,
}

fn only_true<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    if bool::deserialize(d)? {
        Ok(true)
    } else {
        Err(serde::de::Error::custom("none must be true"))
    }
}

#[derive(Deserialize)]
struct RecordBody {
    record: WorkRecord,
}

#[derive(Deserialize)]
struct PostBody {
    /// The item the agent holds.
    item: ItemId,
    /// The attempt the claim was on, from the claim's reply. With it a retry
    /// after the record landed answers `replayed: true`.
    attempt: Option<u32>,
    /// The offset of the claim's `claimed` record, from the claim's reply. It
    /// names the claim exactly, and wins over `attempt`.
    claim: Option<u64>,
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
    /// What each model used during the attempt; the tracker prices it.
    #[serde(default)]
    usage: Vec<Usage>,
    /// Replaced by `usage`: refused, so an agent built for it learns why.
    cost_micros: Option<u64>,
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
    /// Replaces triage's dependencies. Needs `proposal` and format 17.
    depends_on: Option<Vec<ItemId>>,
    /// Replaces triage's split titles. Needs `proposal` and format 17.
    split: Option<Vec<String>>,
    /// The offset of triage's answer the person read, as the item's `proposal.at`.
    proposal: Option<u64>,
}

#[derive(Deserialize)]
struct NoteBody {
    item: ItemId,
    /// The path it steers; none for the item as a whole.
    attempt: Option<u32>,
    #[serde(default = "a_note")]
    input: HumanKind,
    text: Content,
}

fn a_note() -> HumanKind {
    HumanKind::Note
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
    let filter = match body.filter.parse() {
        Ok(f) => f,
        Err(e) => return reply(Err(e)),
    };
    reply(
        call.tracker()
            .next_filtered(&agent, wait, &body.review_tiers, &filter)
            .await,
    )
}

async fn peek<B: Broker>(call: Call<B>, Json(body): Json<PeekBody>) -> Response {
    let agent = match call.agent() {
        Ok(a) => a,
        Err(d) => return denied(d),
    };
    let filter = match body.filter.parse() {
        Ok(f) => f,
        Err(e) => return reply(Err(e)),
    };
    reply(
        call.tracker()
            .peek(
                &agent,
                body.limit.unwrap_or(10),
                &body.review_tiers,
                &filter,
            )
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
    let usage = match usage_of(body.usage, body.cost_micros) {
        Ok(u) => u,
        Err(e) => return reply(Err(e)),
    };
    match call.agent() {
        Ok(a) => reply(
            call.tracker()
                .complete(&a, &body.item, claim_of(body.claim, body.attempt), usage)
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn release<B: Broker>(call: Call<B>, Json(body): Json<ReleaseBody>) -> Response {
    let usage = match usage_of(body.usage, body.cost_micros) {
        Ok(u) => u,
        Err(e) => return reply(Err(e)),
    };
    match call.agent() {
        Ok(a) => reply(
            call.tracker()
                .release(
                    &a,
                    &body.item,
                    claim_of(body.claim, body.attempt),
                    body.reason,
                    body.why,
                    usage,
                )
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn give_up<B: Broker>(call: Call<B>, Json(body): Json<GiveUpBody>) -> Response {
    let usage = match usage_of(body.usage, body.cost_micros) {
        Ok(u) => u,
        Err(e) => return reply(Err(e)),
    };
    match call.agent() {
        Ok(a) => reply(
            call.tracker()
                .give_up(
                    &a,
                    &body.item,
                    claim_of(body.claim, body.attempt),
                    body.reason,
                    usage,
                )
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn records<B: Broker>(call: Call<B>, Keyed(body, rk): Keyed<RecordBody>) -> Response {
    as_person(&call, rk, body.record).await
}

async fn work_post<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<PostBody>) -> Response {
    let a = match call.agent() {
        Ok(a) => a,
        Err(d) => return denied(d),
    };
    if b.work == "prerequisite" {
        // Its retry is replayed by the claim, and its reply says whether the claim goes on.
        if rk.is_some() {
            return reply(Err(Error::Invalid(
                "a prerequisite is retried by its claim; send no request_key".into(),
            )));
        }
        let usage = match usage_of(b.usage, b.cost_micros) {
            Ok(u) => u,
            Err(e) => return reply(Err(e)),
        };
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
                .prerequisite(
                    &a,
                    &b.item,
                    claim_of(b.claim, b.attempt),
                    b.key,
                    which,
                    b.done_when,
                    usage,
                )
                .await,
        );
    }
    let kind = match Post::parse(&b.work) {
        Ok(k) => k,
        Err(e) => return reply(Err(e)),
    };
    reply(
        call.keyed(rk, call.tracker().post(&a, &b.item, b.key, kind, b.title))
            .await,
    )
}

async fn link<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<LinkBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.keyed(rk, call.tracker().link(&a, &b.item, &b.to))
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn context_read<B: Broker>(call: Call<B>, Json(b): Json<ItemBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(call.tracker().context_read(&a, &b.item).await),
        Err(d) => denied(d),
    }
}

async fn context_add<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<ContextAddBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.keyed(rk, call.tracker().context_add(&a, &b.item, b.entry, b.text))
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn progress<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<ProgressBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.keyed(
                rk,
                call.tracker().progress(&a, &b.item, b.note, b.branch, b.pr),
            )
            .await,
        ),
        Err(d) => denied(d),
    }
}

async fn decide<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<DecideBody>) -> Response {
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
    reply(call.keyed(rk, call.tracker().decide(&a, &b.item, d)).await)
}

async fn finding<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<FindingBody>) -> Response {
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
    reply(call.keyed(rk, call.tracker().finding(&a, &b.item, f)).await)
}

async fn review_verdict<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<VerdictBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.keyed(
                rk,
                call.tracker().review_verdict(&a, &b.item, b.verdict, b.why),
            )
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

async fn triage<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<TriageBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.keyed(
                rk,
                call.tracker().triage(
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
                ),
            )
            .await,
        ),
        Err(d) => denied(d),
    }
}

async fn step<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<StepBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.keyed(
                rk,
                call.tracker()
                    .record_step(&a, &b.item, b.command, b.exit_code, b.output),
            )
            .await,
        ),
        Err(d) => denied(d),
    }
}

async fn edit<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<EditBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(
            call.keyed(
                rk,
                call.tracker().edit(&a, &b.item, b.file, b.hunk, b.decision),
            )
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
    /// The claim's attempt and `claimed` offset, as for `release`: a stale
    /// session's question cannot end a newer claim.
    attempt: Option<u32>,
    claim: Option<u64>,
    /// What the attempt cost so far. Asking ends the hold, so this is the last chance to report it.
    /// What each model used during the attempt; the tracker prices it.
    #[serde(default)]
    usage: Vec<Usage>,
    /// Replaced by `usage`: refused, so an agent built for it learns why.
    cost_micros: Option<u64>,
}

async fn ask<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<AskBody>) -> Response {
    let usage = match usage_of(b.usage, b.cost_micros) {
        Ok(u) => u,
        Err(e) => return reply(Err(e)),
    };
    match call.agent() {
        Ok(a) => reply(
            call.keyed(
                rk,
                call.tracker().ask(
                    &a,
                    &b.item,
                    claim_of(b.claim, b.attempt),
                    b.text,
                    b.options,
                    b.default,
                    usage,
                ),
            )
            .await,
        ),
        Err(d) => denied(d),
    }
}

async fn answer<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<AnswerBody>) -> Response {
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
        call.keyed(
            rk,
            call.tracker()
                .answer(&a, &b.item, question, b.text, b.evidence, b.chosen),
        )
        .await,
    )
}

/// A person's action that is one record. Every record a person writes
/// passes here, so the assignee check holds for `/v1/work/records` too: the
/// fold does not read the org log, so it cannot tell who is in the team.
async fn as_person<B: Broker>(
    call: &Call<B>,
    rk: Option<RequestKey>,
    record: WorkRecord,
) -> Response {
    let p = match call.person() {
        Ok(p) => p,
        Err(d) => return denied(d),
    };
    let outsider = match &record {
        WorkRecord::Reassigned {
            to: Some(Assignee::Person(to)),
            ..
        } => (!call.teams.is_member(&call.team, to)).then_some("that person is not in this team"),
        // A revoked agent counts as outside: it can never claim the item.
        WorkRecord::Reassigned {
            to: Some(Assignee::Agent(to)),
            ..
        } => {
            (!call.teams.is_team_agent(&call.team, to)).then_some("that agent is not in this team")
        }
        _ => None,
    };
    if let Some(why) = outsider {
        return refused(StatusCode::CONFLICT, why, "not_member");
    }
    reply(
        call.keyed(rk, call.tracker().person_record(p, record))
            .await,
    )
}

async fn accept<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<AcceptBody>) -> Response {
    let record = WorkRecord::Accepted {
        item: b.item,
        tier: b.tier,
        triage_skipped: None,
    };
    as_person(&call, rk, record).await
}

async fn edit_proposal<B: Broker>(
    call: Call<B>,
    Keyed(b, rk): Keyed<EditProposalBody>,
) -> Response {
    let record = WorkRecord::Edited {
        item: b.item,
        title: b.title,
        tier: b.tier,
        depends_on: b.depends_on,
        split: b.split,
        proposal: b.proposal,
    };
    as_person(&call, rk, record).await
}

async fn reject<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<ItemBody>) -> Response {
    as_person(&call, rk, WorkRecord::Rejected { item: b.item }).await
}

async fn person_answer<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<AnswerBody>) -> Response {
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
            call.keyed(
                rk,
                call.tracker()
                    .person_answer(p, &b.item, b.question, b.text, b.evidence, how),
            )
            .await,
        ),
        Err(d) => denied(d),
    }
}

/// A person's note, review comment or priority change, as `human_input`. The
/// log records which person wrote it. People paste tokens too, so the text
/// is redacted like an agent's.
async fn note<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<NoteBody>) -> Response {
    let mut record = WorkRecord::HumanInput {
        item: b.item,
        attempt: b.attempt,
        input: b.input,
        text: b.text,
    };
    kbl_core::redact::redact_record(&mut record);
    as_person(&call, rk, record).await
}

async fn tier<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<TierBody>) -> Response {
    let record = WorkRecord::TierChanged {
        item: b.item,
        tier: b.tier,
    };
    as_person(&call, rk, record).await
}

async fn close<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<CloseBody>) -> Response {
    let record = WorkRecord::Closed {
        item: b.item,
        why: b.why,
    };
    as_person(&call, rk, record).await
}

async fn unlink<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<LinkBody>) -> Response {
    let record = WorkRecord::Unlinked {
        item: b.item,
        to: b.to,
    };
    as_person(&call, rk, record).await
}

async fn budget<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<BudgetBody>) -> Response {
    let record = WorkRecord::BudgetSet {
        root: b.root,
        items_left: b.items_left,
        max_depth: b.max_depth,
        cost_left_micros: b.cost_left_micros,
        redos_per_item: b.redos_per_item,
    };
    as_person(&call, rk, record).await
}

async fn price<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<PriceBody>) -> Response {
    let record = WorkRecord::PriceSet {
        model: b.model,
        unit: b.unit,
        price_micros: b.price_micros,
        effective_from_ms: b.effective_from_ms,
        as_of: b.as_of,
    };
    let Some(e) = b.expected_price else {
        return as_person(&call, rk, record).await;
    };
    let expected = match e {
        ExpectedPriceBody::Seen(s) => ExpectedPrice::Seen {
            price_micros: s.price_micros,
            since_ms: s.since_ms,
        },
        ExpectedPriceBody::None(_) => ExpectedPrice::None,
    };
    match call.person() {
        Ok(p) => reply(
            call.keyed(rk, call.tracker().person_price(p, record, expected))
                .await,
        ),
        Err(d) => denied(d),
    }
}

async fn rules<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<RulesBody>) -> Response {
    let record = WorkRecord::RulesSet {
        target: Target {
            source: b.source,
            repo: b.repo,
        },
        root: b.root,
        text: b.text,
        checks: b.checks,
    };
    as_person(&call, rk, record).await
}

async fn rules_read<B: Broker>(call: Call<B>, Json(b): Json<ItemBody>) -> Response {
    match call.agent() {
        Ok(a) => reply(call.tracker().rules_read(&a, &b.item).await),
        Err(d) => denied(d),
    }
}

async fn flag<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<ItemBody>) -> Response {
    let record = WorkRecord::Flagged {
        item: b.item,
        flagged: true,
    };
    as_person(&call, rk, record).await
}

async fn unflag<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<ItemBody>) -> Response {
    let record = WorkRecord::Flagged {
        item: b.item,
        flagged: false,
    };
    as_person(&call, rk, record).await
}

async fn retry<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<ItemBody>) -> Response {
    as_person(&call, rk, WorkRecord::Retried { item: b.item }).await
}

async fn snooze<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<SnoozeBody>) -> Response {
    let record = WorkRecord::Snoozed {
        item: b.item,
        until_ms: b.until_ms,
    };
    as_person(&call, rk, record).await
}

async fn park<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<ItemBody>) -> Response {
    let record = WorkRecord::Parked {
        item: b.item,
        parked: true,
    };
    as_person(&call, rk, record).await
}

async fn unpark<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<ItemBody>) -> Response {
    let record = WorkRecord::Parked {
        item: b.item,
        parked: false,
    };
    as_person(&call, rk, record).await
}

async fn unblock<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<UnblockBody>) -> Response {
    let record = WorkRecord::Unblocked {
        item: b.item,
        to: b.to,
    };
    as_person(&call, rk, record).await
}

async fn request_review<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<ReviewBody>) -> Response {
    let record = WorkRecord::ReviewRequested {
        item: b.item,
        reviewer: b.reviewer,
    };
    as_person(&call, rk, record).await
}

async fn request_changes<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<ItemBody>) -> Response {
    as_person(&call, rk, WorkRecord::ChangesRequested { item: b.item }).await
}

async fn mark_merged<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<ItemBody>) -> Response {
    as_person(&call, rk, WorkRecord::MergeMarked { item: b.item }).await
}

async fn rate_summary<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<RateBody>) -> Response {
    let record = WorkRecord::SummaryRated {
        item: b.item,
        attempt: b.attempt,
        accurate: b.accurate,
    };
    as_person(&call, rk, record).await
}

async fn take<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<ItemBody>) -> Response {
    as_person(&call, rk, WorkRecord::Taken { item: b.item }).await
}

/// The fold does not read the org log, so membership is checked here.
async fn reassign<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<ReassignBody>) -> Response {
    let record = WorkRecord::Reassigned {
        item: b.item,
        to: b.to,
    };
    as_person(&call, rk, record).await
}

async fn steer<B: Broker>(call: Call<B>, Keyed(b, rk): Keyed<SteerBody>) -> Response {
    let record = WorkRecord::Steered {
        item: b.item,
        attempt: b.attempt,
        text: b.text,
    };
    as_person(&call, rk, record).await
}

#[derive(Deserialize)]
struct ItemsQuery {
    stage: Option<Stage>,
}

#[derive(Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    from: u64,
}

#[derive(Deserialize)]
struct WindowQuery {
    days: Option<u64>,
}

async fn time_to_human<B: Broker>(call: Call<B>, Query(q): Query<WindowQuery>) -> Response {
    let days = q
        .days
        .unwrap_or(30)
        .clamp(1, crate::tracker::reads::MAX_WINDOW_DAYS);
    match call.person() {
        Ok(_) => reply(call.tracker().time_to_human_action(days).await),
        Err(d) => denied(d),
    }
}

async fn summary_trust<B: Broker>(call: Call<B>, Query(q): Query<WindowQuery>) -> Response {
    let days = q
        .days
        .unwrap_or(30)
        .clamp(1, crate::tracker::reads::MAX_WINDOW_DAYS);
    match call.person() {
        Ok(_) => reply(call.tracker().summary_trust(days).await),
        Err(d) => denied(d),
    }
}

/// Entries My queue returns when `limit` is not given, and the most it returns.
const QUEUE_DEFAULT_LIMIT: usize = 200;
const QUEUE_MAX_LIMIT: usize = 1000;

#[derive(Deserialize)]
struct QueueQuery {
    limit: Option<usize>,
}

async fn queue<B: Broker>(call: Call<B>, Query(q): Query<QueueQuery>) -> Response {
    match call.person() {
        Ok(p) => {
            let limit = q.limit.unwrap_or(QUEUE_DEFAULT_LIMIT).min(QUEUE_MAX_LIMIT);
            reply(call.tracker().queue(&p, limit).await)
        }
        Err(d) => denied(d),
    }
}

async fn items<B: Broker>(call: Call<B>, Query(q): Query<ItemsQuery>) -> Response {
    match call.person() {
        Ok(_) => reply(call.tracker().items(q.stage).await),
        Err(d) => denied(d),
    }
}

/// The team's agents a person may assign an item to: the token file's and
/// the minted ones not revoked. Names only, so any member may read it.
async fn team_agents<B: Broker>(call: Call<B>) -> Response {
    if let Err(d) = call.person() {
        return denied(d);
    }
    let mut names: BTreeSet<String> = (call.team.file_agents().into_iter())
        .map(|a| a.as_str().to_owned())
        .collect();
    if let Some(org) = call.teams.org() {
        let state = org.org.state().await;
        names.extend(
            state
                .agents_in(call.team.name())
                .filter(|(_, a)| !a.revoked)
                .map(|(n, _)| n.as_str().to_owned()),
        );
    }
    Json(json!({ "agents": names })).into_response()
}

async fn item<B: Broker>(call: Call<B>, Path(id): Path<ItemId>) -> Response {
    match call.person() {
        Ok(_) => reply(call.tracker().item(&id).await),
        Err(d) => denied(d),
    }
}

async fn history<B: Broker>(
    call: Call<B>,
    Path(id): Path<ItemId>,
    Query(q): Query<HistoryQuery>,
) -> Response {
    match call.person() {
        Ok(_) => reply(call.tracker().history(&id, q.from).await),
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

/// The token is not in the token file, and the org view cannot say whether it
/// is a live agent's until this tracker holds the org lease again.
fn org_unavailable() -> Response {
    let mut res = refused(
        StatusCode::SERVICE_UNAVAILABLE,
        "this tracker does not hold the org writer lease; agent tokens wait for it",
        "org_unavailable",
    );
    res.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    res
}

#[cfg(test)]
mod tests;
