//! `org init` and `team create`: the Felix side of teams, and the call that
//! tells the tracker to start serving one. See docs/design.md, "Teams".

use anyhow::{Context, Result, bail};
use clap::Args;
use kbl_core::org::{ORG_EVENTS, ORG_NAMESPACE, ORG_TRACKER_CACHE, TeamName, TokenHash};
use serde_json::{Value, json};

use crate::felix::{Session, Target};

/// The role that may manage every namespace the tracker serves.
const TRACKER_ROLE: &str = "role:kbl-tracker";

#[derive(Args)]
pub(crate) struct InitArgs {
    /// The tracker's subject in the identity provider.
    #[arg(long)]
    tracker_subject: String,
    /// The identity provider's issuer. Defaults to `--idp`.
    #[arg(long)]
    issuer: Option<String>,
}

#[derive(Args)]
pub(crate) struct CreateTeamArgs {
    /// The team's name, which is also its Felix namespace.
    team: String,
    #[arg(long)]
    display_name: Option<String>,
    #[arg(long, env = "KBL_TRACKER_URL", default_value = "http://127.0.0.1:8790")]
    tracker_url: String,
    /// An organization admin's tracker token.
    #[arg(long, env = "KBL_TRACKER_TOKEN", hide_env_values = true)]
    tracker_token: String,
}

/// The id Felix keys a person or service on: sha256 of `issuer|subject`.
pub(crate) fn principal(issuer: &str, subject: &str) -> String {
    TokenHash::of(&format!("{issuer}|{subject}"))
        .as_str()
        .to_owned()
}

fn namespace_body(name: &str) -> Value {
    json!({ "namespace": name, "display_name": name })
}

fn policy(subject: &str, object: &str, action: &str) -> Value {
    json!({ "subject": subject, "object": object, "action": action })
}

/// What the tracker may manage: the organization's namespace and the first team's.
fn tracker_policies(tenant: &str, namespaces: &[&str]) -> Vec<Value> {
    namespaces
        .iter()
        .map(|ns| {
            policy(
                TRACKER_ROLE,
                &format!("namespace:{tenant}/{ns}"),
                "ns.manage",
            )
        })
        .collect()
}

fn viewer_role(team: &str) -> String {
    format!("role:kbl-{team}-viewer")
}

/// The grants a team gets from `team create`.
fn team_policies(tenant: &str, team: &str) -> Vec<Value> {
    let mut policies = tracker_policies(tenant, &[team]);
    policies.push(policy(
        &viewer_role(team),
        &format!("stream:{tenant}/{team}/{}", kbl_core::work::WORK_EVENTS),
        "stream.subscribe",
    ));
    policies
}

/// The organization's log and the tracker's cache, shaped like the tracker's own.
fn org_batch() -> Value {
    json!({
        "streams": [{
            "stream": ORG_EVENTS,
            "kind": "Stream",
            "shards": 1,
            "replication_factor": 1,
            "retention": { "max_age_seconds": null, "max_size_bytes": null },
            "consistency": "Leader",
            "delivery": "AtLeastOnce",
            "durable": true,
        }],
        "caches": [{
            "cache": ORG_TRACKER_CACHE,
            "display_name": ORG_TRACKER_CACHE,
            "shards": 1,
            "replication_factor": 1,
            "consistency": "Leader",
        }],
    })
}

fn team_name(name: &str) -> Result<TeamName> {
    TeamName::new(name).map_err(|_| {
        anyhow::anyhow!(
            "`{name}` is not a team name: use letters, digits, `-` or `_`, and not `{ORG_NAMESPACE}`, which holds the organization"
        )
    })
}

pub(crate) async fn org_init(target: Target, args: InitArgs) -> Result<()> {
    team_name(&target.namespace).context("the first team's namespace (--namespace)")?;
    let tenant = target.tenant.clone();
    let first = target.namespace.clone();
    let issuer = args
        .issuer
        .unwrap_or_else(|| target.idp.clone())
        .trim_end_matches('/')
        .to_owned();
    let session = Session::sign_in(target).await?;
    let token = session.exchange("felix-controlplane").await?;
    let who = principal(&issuer, &args.tracker_subject);
    let mut steps = vec![
        (
            "namespaces".to_owned(),
            namespace_body(ORG_NAMESPACE),
            "create the organization's namespace",
        ),
        (
            format!("namespaces/{ORG_NAMESPACE}/resources"),
            org_batch(),
            "create the organization's log",
        ),
    ];
    for body in tracker_policies(&tenant, &[ORG_NAMESPACE, &first]) {
        steps.push((
            "rbac/policies".to_owned(),
            body,
            "grant the tracker its namespaces",
        ));
    }
    steps.push((
        "rbac/groupings".to_owned(),
        json!({ "user": who, "role": TRACKER_ROLE }),
        "give the tracker its role",
    ));
    for (path, body, doing) in steps {
        session
            .control_plane_post(&token, &path, &body, doing)
            .await?;
    }

    println!("organization ready on tenant {tenant}");
    println!("  namespace {ORG_NAMESPACE}: {ORG_EVENTS}, cache {ORG_TRACKER_CACHE}");
    println!(
        "  {} may manage {ORG_NAMESPACE} and {first}",
        args.tracker_subject
    );
    println!("next: restart the tracker with KBL_ORG_NAMESPACE={ORG_NAMESPACE}");
    Ok(())
}

pub(crate) async fn team_create(target: Target, args: CreateTeamArgs) -> Result<()> {
    // Checked before Felix is called, so a bad name leaves nothing behind.
    let team = team_name(&args.team)?;
    let team = team.as_str();
    let tenant = target.tenant.clone();
    let session = Session::sign_in(target).await?;
    let token = session.exchange("felix-controlplane").await?;

    session
        .control_plane_post(
            &token,
            "namespaces",
            &namespace_body(team),
            "create the team's namespace",
        )
        .await?;
    for body in team_policies(&tenant, team) {
        session
            .control_plane_post(&token, "rbac/policies", &body, "grant the team's access")
            .await?;
    }

    let mut request = json!({ "team": team });
    if let Some(name) = &args.display_name {
        request["display_name"] = json!(name);
    }
    let url = format!("{}/v1/org/teams", args.tracker_url.trim_end_matches('/'));
    let response = session
        .http()
        .post(url)
        .bearer_auth(&args.tracker_token)
        .json(&request)
        .send()
        .await
        .context("could not reach the tracker")?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let error = body["error"].as_str().unwrap_or("no reason given");
        match body["why"].as_str() {
            Some(why) => bail!("the tracker refused: {status}: {error} ({why})"),
            None => bail!("the tracker refused: {status}: {error}"),
        }
    }
    if body["created"].as_bool() == Some(true) {
        println!("created team {team}");
    } else {
        println!("team {team} already exists");
    }
    if body["serving"].as_bool() == Some(true) {
        println!("  serving");
    } else {
        println!("  not serving yet: the tracker will keep trying to start it");
    }
    println!(
        "dashboard: until felix-gateway takes the namespace from the scope, run a gateway with GATEWAY_NAMESPACE={team}"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
