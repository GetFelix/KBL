//! The org routes: the teams of the org, for org admins. A team is created on
//! `org.events` first and started second, so a tracker that stops in between
//! starts it from the log on its next start.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use kbl_core::names::Key;
use kbl_core::org::{OrgRecord, TeamName};
use kbl_core::org_fold::Applied;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{App, bearer, denied, refused};
use crate::felix::Broker;
use crate::org::OrgError;
use crate::teams::OrgSide;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateBody {
    team: String,
    display_name: Option<String>,
}

/// `GET /v1/org/teams`: every team, and whether it is serving.
pub(super) async fn list_teams<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
) -> Response {
    let org = match org_admin(&app, &headers) {
        Ok((org, _)) => org,
        Err(res) => return *res,
    };
    let state = org.org.state().await;
    let teams: Vec<Value> = state
        .teams()
        .map(|(name, team)| {
            let serving = app.teams.find(name).is_some_and(|t| t.tracker().is_some());
            json!({
                "team": name,
                "display_name": team.display_name,
                "adopted": team.adopted,
                "serving": serving,
            })
        })
        .collect();
    Json(json!({ "teams": teams })).into_response()
}

/// `POST /v1/org/teams`: writes `team_created` and starts serving the team.
/// A team the log already has answers `created: false` and is started if it
/// is not served yet, so a create that stopped part way finishes on a retry.
pub(super) async fn create_team<B: Broker>(
    State(app): State<Arc<App<B>>>,
    headers: HeaderMap,
    body: Result<Json<CreateBody>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let (org, by) = match org_admin(&app, &headers) {
        Ok(found) => found,
        Err(res) => return *res,
    };
    let Ok(Json(body)) = body else {
        return refused(StatusCode::BAD_REQUEST, "the body is not a team", "invalid");
    };
    let Ok(team) = TeamName::new(body.team) else {
        return refused(
            StatusCode::BAD_REQUEST,
            "a team name is 1 to 64 of [A-Za-z0-9_-] and not `kbl`",
            "invalid_team",
        );
    };
    let (created, offset) = if org.org.state().await.has_team(&team) {
        (false, None)
    } else {
        let record = OrgRecord::TeamCreated {
            team: team.clone(),
            display_name: body.display_name,
            adopted: false,
        };
        match org.org.append(by, record).await {
            Ok((applied, offset)) => (applied == Applied::Changed, offset),
            Err(OrgError::Refused(why)) => {
                return refused(StatusCode::CONFLICT, "the org log refused the team", why);
            }
            Err(OrgError::Unavailable(e)) => {
                return refused(StatusCode::SERVICE_UNAVAILABLE, &e, "org_unavailable");
            }
            Err(OrgError::Felix(e)) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({ "error": format!("{e:#}") })),
                )
                    .into_response();
            }
        }
    };
    let serving = app.teams.ensure(&team).await;
    Json(json!({
        "team": team,
        "created": created,
        "offset": offset,
        "serving": serving,
    }))
    .into_response()
}

/// The org side and the org admin calling, or the answer that refuses the call.
fn org_admin<'a, B: Broker>(
    app: &'a Arc<App<B>>,
    headers: &HeaderMap,
) -> Result<(&'a OrgSide<B>, Key), Box<Response>> {
    let Some(org) = app.teams.org() else {
        return Err(Box::new(refused(
            StatusCode::NOT_FOUND,
            "this tracker runs without teams: set KBL_ORG_NAMESPACE",
            "teams_off",
        )));
    };
    let unknown = || {
        Box::new(denied((
            StatusCode::UNAUTHORIZED,
            "unknown or missing token",
        )))
    };
    let Some(token) = bearer(headers) else {
        return Err(unknown());
    };
    if let Some(person) = app.teams.org_admin(token) {
        return Ok((org, person));
    }
    if app.teams.who(token).is_empty() {
        Err(unknown())
    } else {
        Err(Box::new(refused(
            StatusCode::FORBIDDEN,
            "this call is for an org admin",
            "org_admin_required",
        )))
    }
}
