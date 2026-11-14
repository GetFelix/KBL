//! The agent routes: `agent add`, `list` and `revoke`. An agent added here is
//! on `org.events` as a token hash bound to one team, and its token works on
//! the next call. Revoking it releases what it holds, as a lapsed claim is.

use std::sync::Arc;

use axum::Json;
use axum::body::to_bytes;
use axum::extract::Request;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use kbl_core::names::{AgentName, Key};
use kbl_core::org::{OrgRecord, TeamName, TokenHash};
use kbl_core::org_fold::Applied;
use serde::Deserialize;
use serde_json::{Value, json};

use super::org::org_admin;
use super::{App, refused};
use crate::felix::Broker;
use crate::org::OrgError;
use crate::teams::Team;

/// An agent name fits in far less.
const MAX_BODY: usize = 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddBody {
    agent: String,
}

/// Serves `/v1/teams/{team}/agents` and `/v1/teams/{team}/agents/{agent}`.
pub(super) async fn route<B: Broker>(
    app: Arc<App<B>>,
    team: &str,
    rest: &str,
    req: Request,
) -> Response {
    let agent = rest.strip_prefix("agents/");
    match (req.method().clone(), agent) {
        (Method::POST, None) => add(&app, team, req).await,
        (Method::GET, None) => list(&app, team, req.headers()).await,
        (Method::DELETE, Some(agent)) => revoke(&app, team, agent, req.headers()).await,
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// The team the path names, for an org admin; or the answer that refuses the call.
fn admin_in<B: Broker>(
    app: &Arc<App<B>>,
    team: &str,
    headers: &HeaderMap,
) -> Result<(Arc<Team<B>>, Key), Box<Response>> {
    let (_, by) = org_admin(app, headers)?;
    let team = TeamName::new(team)
        .ok()
        .and_then(|t| app.teams.find(&t))
        .ok_or_else(|| {
            Box::new(refused(
                StatusCode::NOT_FOUND,
                "no such team",
                "unknown_team",
            ))
        })?;
    Ok((team, by))
}

/// `POST /v1/teams/{team}/agents`: mints a token, writes `agent_added` with
/// its hash, and answers the token. It is not kept and is never shown again.
async fn add<B: Broker>(app: &Arc<App<B>>, team: &str, req: Request) -> Response {
    // The caller is checked before the body is read, and the body is small.
    let (team, by) = match admin_in(app, team, req.headers()) {
        Ok(found) => found,
        Err(res) => return *res,
    };
    let body = match to_bytes(req.into_body(), MAX_BODY).await {
        Ok(bytes) => serde_json::from_slice::<AddBody>(&bytes).ok(),
        Err(_) => None,
    };
    let Some(body) = body else {
        return refused(
            StatusCode::BAD_REQUEST,
            "the body is not an agent",
            "invalid",
        );
    };
    let Ok(agent) = AgentName::new(body.agent) else {
        return refused(
            StatusCode::BAD_REQUEST,
            "an agent name is 1 to 64 of [A-Za-z0-9_], and not `plan` or `shared`",
            "invalid_agent",
        );
    };
    // Two tokens for one name would claim as one consumer-group member.
    if team.has_file_agent(agent.as_str()) {
        return refused(
            StatusCode::CONFLICT,
            "the token file already names this agent",
            "name_taken",
        );
    }
    let token = match mint() {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("no random token: {e}") })),
            )
                .into_response();
        }
    };
    let record = OrgRecord::AgentAdded {
        team: team.name().clone(),
        agent: Key::new(agent.as_str()).expect("an agent name is a key"),
        token_sha256: TokenHash::of(&token),
    };
    let org = app.teams.org().expect("org_admin found the org");
    match org.org.append(by, record).await {
        Ok((Applied::Changed, offset)) => Json(json!({
            "team": team.name(),
            "agent": agent,
            "token": token,
            "offset": offset,
        }))
        .into_response(),
        // A fresh token cannot match one on the log, so this is a name in use.
        Ok((Applied::Unchanged, _)) => {
            refused(StatusCode::CONFLICT, "the agent exists", "name_taken")
        }
        Err(e) => org_error(
            e,
            "the agent may or may not have been added; run `agent list`, and if it is there, revoke it and add a new name",
        ),
    }
}

/// `GET /v1/teams/{team}/agents`: every agent of the team, revoked ones too,
/// with what each holds.
async fn list<B: Broker>(app: &Arc<App<B>>, team: &str, headers: &HeaderMap) -> Response {
    let (team, _) = match admin_in(app, team, headers) {
        Ok(found) => found,
        Err(res) => return *res,
    };
    let org = app.teams.org().expect("org_admin found the org");
    let state = org.org.state().await;
    let tracker = team.tracker();
    let mut agents: Vec<Value> = Vec::new();
    for (name, agent) in state.agents_in(team.name()) {
        let holds = match (&tracker, AgentName::new(name.as_str())) {
            (Some(t), Ok(a)) if !agent.revoked => Some(t.holds_of(&a).await),
            _ => None,
        };
        agents.push(json!({
            "agent": name,
            "added_by": agent.added_by,
            "revoked": agent.revoked,
            "holds": holds,
        }));
    }
    for name in team.file_agents() {
        // A name revoked while minted, then put back in the token file, is one
        // agent: show the file row and mark the revoked minted token.
        let before = agents.len();
        agents.retain(|a| !(a["agent"] == name.as_str() && a["revoked"] == true));
        let revoked_minted = agents.len() != before;
        let holds = match &tracker {
            Some(t) => Some(t.holds_of(&name).await),
            None => None,
        };
        agents.push(json!({
            "agent": name,
            "token_file": true,
            "revoked": false,
            "revoked_minted": revoked_minted,
            "holds": holds,
        }));
    }
    Json(json!({ "team": team.name(), "agents": agents })).into_response()
}

/// `DELETE /v1/teams/{team}/agents/{agent}`: writes `agent_revoked`, so the
/// token stops working on the next call, then releases the agent's holds.
/// A retry writes nothing and releases anything still held.
async fn revoke<B: Broker>(
    app: &Arc<App<B>>,
    team: &str,
    agent: &str,
    headers: &HeaderMap,
) -> Response {
    let (team, by) = match admin_in(app, team, headers) {
        Ok(found) => found,
        Err(res) => return *res,
    };
    let Ok(agent) = AgentName::new(agent) else {
        return refused(StatusCode::NOT_FOUND, "no such agent", "unknown_agent");
    };
    // The token file naming it too must not stop a minted token's revoke.
    let in_file = team.has_file_agent(agent.as_str());
    let record = OrgRecord::AgentRevoked {
        team: team.name().clone(),
        agent: Key::new(agent.as_str()).expect("an agent name is a key"),
    };
    let org = app.teams.org().expect("org_admin found the org");
    let (applied, offset) = match org.org.append(by, record).await {
        Ok(done) => done,
        Err(OrgError::Refused("unknown_agent")) if in_file => {
            return refused(
                StatusCode::CONFLICT,
                "this agent is in the token file; remove it there and restart the tracker",
                "in_token_file",
            );
        }
        Err(OrgError::Refused("unknown_agent")) => {
            return refused(StatusCode::NOT_FOUND, "no such agent", "unknown_agent");
        }
        Err(e) => return org_error(e, "the agent may or may not be revoked; run this again"),
    };
    // The token is refused from here on, so nothing new can be claimed under it.
    let (released, note) = match team.tracker() {
        // Holds go by name, and the name is also the token file's live agent.
        Some(_) if in_file => (
            None,
            Some("the token file names this agent too; its holds stay".to_owned()),
        ),
        Some(t) => match t.revoke(&agent).await {
            Ok(items) => (Some(items), None),
            Err(e) => (
                None,
                Some(format!("its holds were not released yet ({e}); they lapse")),
            ),
        },
        None => (
            None,
            Some("the team is not serving; its holds lapse when it is back".to_owned()),
        ),
    };
    Json(json!({
        "team": team.name(),
        "agent": agent,
        "revoked": applied == Applied::Changed,
        "offset": offset,
        "released": released,
        "note": note,
    }))
    .into_response()
}

fn org_error(e: OrgError, felix_note: &str) -> Response {
    match e {
        OrgError::Refused(why) => {
            refused(StatusCode::CONFLICT, "the org log refused the change", why)
        }
        OrgError::Unavailable(e) => refused(StatusCode::SERVICE_UNAVAILABLE, &e, "org_unavailable"),
        OrgError::Felix(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("{e:#}"), "note": felix_note })),
        )
            .into_response(),
    }
}

/// 32 random bytes as hex. Only the SHA-256 is kept, on `org.events`.
fn mint() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
