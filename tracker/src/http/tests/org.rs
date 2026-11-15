//! The org routes, and a team that is not serving while the others are.

use std::collections::HashMap;

use kbl_core::codec::Codec;
use kbl_core::org::{ORG_EVENTS, OrgLogged, OrgRecord, TokenHash};
use kbl_core::org_fold::Applied;
use kbl_core::work::WORK_EVENTS;

use super::*;
use crate::org::{Org, OrgError};
use crate::teams::{OrgSide, Starter};

/// A tracker with teams on: `default` adopted, `tok-ops` its org admin and
/// `tok-w1` an agent of `default`. Each team gets its own Felix, made on its
/// first start and kept, so a test can break one team's.
struct OrgApp {
    app: Router,
    teams: Arc<Teams<Quiet>>,
    org: Quiet,
    brokers: Arc<StdMutex<HashMap<String, Quiet>>>,
}

impl OrgApp {
    fn broker(&self, team: &str) -> Quiet {
        broker_of(&self.brokers, team)
    }
}

fn broker_of(brokers: &StdMutex<HashMap<String, Quiet>>, team: &str) -> Quiet {
    brokers
        .lock()
        .unwrap()
        .entry(team.to_owned())
        .or_default()
        .clone()
}

fn name(n: &str) -> TeamName {
    TeamName::new(n).unwrap()
}

async fn org_app() -> OrgApp {
    let brokers: Arc<StdMutex<HashMap<String, Quiet>>> = Arc::default();
    let made = brokers.clone();
    let start: Starter<Quiet> = Arc::new(move |team: TeamName| {
        let broker = broker_of(&made, team.as_str());
        Box::pin(async move { Tracker::start(broker, quiet_settings()).await })
    });
    let org_broker = Quiet::default();
    org_broker.keeps.store(true, Ordering::SeqCst);
    let org = Arc::new(
        Org::start(org_broker.clone(), Duration::from_secs(15), Duration::ZERO)
            .await
            .unwrap(),
    );
    let adopt = OrgRecord::TeamCreated {
        team: name("default"),
        display_name: None,
        adopted: true,
    };
    org.append(Key::new("tracker").unwrap(), adopt)
        .await
        .unwrap();
    let tokens = Tokens::parse(r#"{"agents":{"tok-w1":"w1"},"people":{"tok-ops":"ops"}}"#).unwrap();
    let first = Team::new(
        name("default"),
        start(name("default")).await.unwrap(),
        tokens.clone(),
        64,
    );
    let teams = Arc::new(
        Teams::with_org(
            vec![first],
            Some(OrgSide {
                org,
                start,
                admins: tokens.people_only(),
                inflight: 64,
            }),
        )
        .unwrap(),
    );
    OrgApp {
        app: router(teams.clone()),
        teams,
        org: org_broker,
        brokers,
    }
}

async fn create(app: &Router, token: Option<&str>, team: &str) -> (StatusCode, Value) {
    call(app, "/v1/org/teams", token, json!({ "team": team })).await
}

fn why(v: &Value) -> Option<&str> {
    v["why"].as_str()
}

#[tokio::test]
async fn the_org_routes_answer_teams_off_without_teams() {
    let (status, body) = get_json(&app().await, "/v1/org/teams", Some("tok-gab")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(why(&body), Some("teams_off"));
}

#[tokio::test]
async fn only_an_org_admin_creates_a_team_and_a_retry_creates_nothing() {
    let o = org_app().await;
    let (status, body) = create(&o.app, Some("tok-w1"), "alpha").await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::FORBIDDEN, Some("org_admin_required"))
    );
    let (status, _) = create(&o.app, None, "alpha").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = create(&o.app, Some("tok-nobody"), "alpha").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for bad in ["kbl", "a/b", ""] {
        let (status, body) = create(&o.app, Some("tok-ops"), bad).await;
        assert_eq!(
            (status, why(&body)),
            (StatusCode::BAD_REQUEST, Some("invalid_team")),
            "{bad}"
        );
    }
    let big = json!({ "team": "big", "display_name": "x".repeat(8 * 1024) });
    let (status, body) = call(&o.app, "/v1/org/teams", Some("tok-ops"), big).await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::BAD_REQUEST, Some("invalid"))
    );
    // Only the adoption is on the log.
    assert_eq!(
        o.org.published().len(),
        1,
        "a refused create wrote to org.events"
    );

    let (status, body) = create(&o.app, Some("tok-ops"), "alpha").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["created"].as_bool(), body["serving"].as_bool()),
        (Some(true), Some(true))
    );
    assert_eq!(o.org.published().len(), 2);
    let (status, body) = create(&o.app, Some("tok-ops"), "alpha").await;
    assert_eq!(
        (status, body["created"].as_bool()),
        (StatusCode::OK, Some(false))
    );
    let (_, body) = create(&o.app, Some("tok-ops"), "default").await;
    assert_eq!(
        body["created"].as_bool(),
        Some(false),
        "the adopted team is not created again"
    );
    assert_eq!(o.org.published().len(), 2);

    // The new team serves at once, on its own Felix, and only for its members.
    let (status, _) = call(
        &o.app,
        "/v1/teams/alpha/work/records",
        Some("tok-ops"),
        imported("a1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(o.broker("alpha").published(), vec![WORK_EVENTS.to_owned()]);
    assert!(o.broker("default").published().is_empty());
    let (status, body) = call(
        &o.app,
        "/v1/teams/alpha/work/next",
        Some("tok-w1"),
        json!({}),
    )
    .await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::NOT_FOUND, Some("unknown_team"))
    );

    let (status, body) = get_json(&o.app, "/v1/org/teams", Some("tok-ops")).await;
    assert_eq!(status, StatusCode::OK);
    let listed: Vec<(&str, bool, bool)> = body["teams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["team"].as_str().unwrap(),
                t["adopted"].as_bool().unwrap(),
                t["serving"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        vec![("alpha", false, true), ("default", true, true)]
    );
    let (status, _) = get_json(&o.app, "/v1/org/teams", Some("tok-w1")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test(start_paused = true)]
async fn a_team_whose_log_is_newer_answers_503_and_serves_once_it_can_start() {
    let o = org_app().await;
    o.broker("beta")
        .log
        .lock()
        .unwrap()
        .push((0, br#"{"v":99,"kind":"imported"}"#.to_vec()));
    let teams = o.teams.clone();
    let maintain = tokio::spawn(async move { teams.maintain().await });
    let (status, body) = create(&o.app, Some("tok-ops"), "beta").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (body["created"].as_bool(), body["serving"].as_bool()),
        (Some(true), Some(false))
    );
    let (status, body) = call(
        &o.app,
        "/v1/teams/beta/work/records",
        Some("tok-ops"),
        imported("b1"),
    )
    .await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("team_unavailable"))
    );
    let (status, _) = call(
        &o.app,
        "/v1/teams/default/work/records",
        Some("tok-ops"),
        imported("d1"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "another team's newer log stopped this one"
    );

    // An upgrade (here: the record going away) lets the retry start it.
    o.broker("beta").log.lock().unwrap().clear();
    tokio::time::sleep(crate::teams::RETRY_EVERY * 2).await;
    let (status, _) = call(
        &o.app,
        "/v1/teams/beta/work/records",
        Some("tok-ops"),
        imported("b1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!maintain.is_finished());
}

#[tokio::test(start_paused = true)]
async fn a_team_that_lost_its_lease_takes_it_back_without_a_restart() {
    let o = org_app().await;
    create(&o.app, Some("tok-ops"), "alpha").await;
    let teams = o.teams.clone();
    let maintain = tokio::spawn(async move { teams.maintain().await });
    o.broker("alpha").lease_lost.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(60)).await;
    let (status, body) = call(
        &o.app,
        "/v1/teams/alpha/work/records",
        Some("tok-ops"),
        imported("a1"),
    )
    .await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("team_unavailable"))
    );
    let (status, _) = call(
        &o.app,
        "/v1/teams/default/work/records",
        Some("tok-ops"),
        imported("d1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !maintain.is_finished(),
        "one team's lost lease stopped the tracker"
    );

    o.broker("alpha").lease_lost.store(false, Ordering::SeqCst);
    tokio::time::sleep(crate::teams::RETRY_EVERY * 2).await;
    let (status, _) = call(
        &o.app,
        "/v1/teams/alpha/work/records",
        Some("tok-ops"),
        imported("a1"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the team did not take its lease back"
    );
}

#[tokio::test(start_paused = true)]
async fn the_tracker_stops_when_the_org_lease_is_lost_for_its_grace() {
    let o = org_app().await;
    let teams = o.teams.clone();
    let maintain = tokio::spawn(async move { teams.maintain().await });
    o.org.lease_lost.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(30)).await;
    // Org changes stop at once; teams keep serving with what they have.
    let (status, body) = create(&o.app, Some("tok-ops"), "gamma").await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("org_unavailable"))
    );
    let (status, _) = call(
        &o.app,
        "/v1/teams/default/work/records",
        Some("tok-ops"),
        imported("d1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!maintain.is_finished());
    tokio::time::timeout(crate::org::ORG_LEASE_GRACE * 2, maintain)
        .await
        .expect("the tracker stops once the org lease is gone for its grace")
        .unwrap();
}

async fn add_agent(
    app: &Router,
    token: Option<&str>,
    team: &str,
    agent: &str,
) -> (StatusCode, Value) {
    call(
        app,
        &format!("/v1/teams/{team}/agents"),
        token,
        json!({ "agent": agent }),
    )
    .await
}

async fn revoke_agent(
    app: &Router,
    token: Option<&str>,
    team: &str,
    agent: &str,
) -> (StatusCode, Value) {
    let path = format!("/v1/teams/{team}/agents/{agent}");
    send(app, Request::delete(path.as_str()), token, Value::Null).await
}

async fn next_as(app: &Router, token: &str) -> StatusCode {
    call(
        app,
        "/v1/work/next",
        Some(token),
        json!({ "wait_seconds": 0 }),
    )
    .await
    .0
}

fn agent_of(teams: &Teams<Quiet>, token: &str) -> Vec<(String, String)> {
    teams
        .who(token)
        .unwrap()
        .into_iter()
        .map(|(team, caller)| match caller {
            Caller::Agent(a) => (team.name().to_string(), a.as_str().to_owned()),
            Caller::Person(_) | Caller::Ingest(_) => panic!("not an agent"),
        })
        .collect()
}

#[tokio::test]
async fn three_agents_added_with_three_calls_are_three_callers_with_no_restart() {
    let o = org_app().await;
    let mut tokens = Vec::new();
    for name in ["w2", "w3", "w4"] {
        let (status, body) = add_agent(&o.app, Some("tok-ops"), "default", name).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let token = body["token"].as_str().unwrap().to_owned();
        assert_eq!(token.len(), 64);
        assert_eq!(next_as(&o.app, &token).await, StatusCode::OK);
        assert_eq!(
            agent_of(&o.teams, &token),
            [("default".to_owned(), name.to_owned())]
        );
        tokens.push(token);
    }
    let first = tokens[0].clone();
    tokens.sort();
    tokens.dedup();
    assert_eq!(tokens.len(), 3, "two agents got one token");
    let (status, body) = get_json(&o.app, "/v1/teams/default/agents", Some("tok-ops")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let names: Vec<(&str, bool)> = body["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["agent"].as_str().unwrap(),
                a["token_file"].as_bool() == Some(true),
            )
        })
        .collect();
    assert_eq!(
        names,
        [("w2", false), ("w3", false), ("w4", false), ("w1", true)]
    );
    // The org log keeps the token's hash.
    let state = o.teams.org().unwrap().org.state().await;
    let w2 = state
        .agent(&name("default"), &Key::new("w2").unwrap())
        .unwrap();
    assert_eq!(w2.token, kbl_core::org::TokenHash::of(&first));
}

#[tokio::test]
async fn an_added_agent_is_bound_to_its_team() {
    let o = org_app().await;
    assert_eq!(
        create(&o.app, Some("tok-ops"), "alpha").await.0,
        StatusCode::OK
    );
    let (_, body) = add_agent(&o.app, Some("tok-ops"), "alpha", "w2").await;
    let token = body["token"].as_str().unwrap();
    let (status, body) = call(
        &o.app,
        "/v1/teams/default/work/next",
        Some(token),
        json!({ "wait_seconds": 0 }),
    )
    .await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::NOT_FOUND, Some("unknown_team"))
    );
    assert_eq!(
        agent_of(&o.teams, token),
        [("alpha".to_owned(), "w2".to_owned())]
    );
}

#[tokio::test]
async fn only_an_org_admin_adds_lists_or_revokes_agents() {
    let o = org_app().await;
    let (status, body) = add_agent(&o.app, Some("tok-w1"), "default", "w2").await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::FORBIDDEN, Some("org_admin_required"))
    );
    assert_eq!(
        add_agent(&o.app, None, "default", "w2").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        add_agent(&o.app, Some("tok-nobody"), "default", "w2")
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, body) = get_json(&o.app, "/v1/teams/default/agents", Some("tok-w1")).await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::FORBIDDEN, Some("org_admin_required"))
    );
    let (status, body) = revoke_agent(&o.app, Some("tok-w1"), "default", "w1").await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::FORBIDDEN, Some("org_admin_required"))
    );
    let (status, body) = add_agent(&o.app, Some("tok-ops"), "nope", "w2").await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::NOT_FOUND, Some("unknown_team"))
    );
    // Only the adoption is on the log.
    assert_eq!(o.org.published().len(), 1);
    let (status, body) = add_agent(&app().await, Some("tok-gab"), "default", "w2").await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::NOT_FOUND, Some("teams_off"))
    );
}

#[tokio::test]
async fn a_name_in_the_token_file_or_once_revoked_is_not_added_again() {
    let o = org_app().await;
    // Two tokens for one name would claim as one consumer-group member.
    let (status, body) = add_agent(&o.app, Some("tok-ops"), "default", "w1").await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::CONFLICT, Some("name_taken"))
    );
    for bad in ["a-b", "plan", ""] {
        let (status, body) = add_agent(&o.app, Some("tok-ops"), "default", bad).await;
        assert_eq!(
            (status, why(&body)),
            (StatusCode::BAD_REQUEST, Some("invalid_agent")),
            "{bad}"
        );
    }
    assert_eq!(
        add_agent(&o.app, Some("tok-ops"), "default", "w2").await.0,
        StatusCode::OK
    );
    let (status, body) = add_agent(&o.app, Some("tok-ops"), "default", "w2").await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::CONFLICT, Some("name_taken"))
    );
    assert_eq!(
        revoke_agent(&o.app, Some("tok-ops"), "default", "w2")
            .await
            .0,
        StatusCode::OK
    );
    let (status, body) = add_agent(&o.app, Some("tok-ops"), "default", "w2").await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::CONFLICT, Some("name_taken"))
    );
}

#[tokio::test]
async fn a_revoked_token_is_refused_on_its_next_call() {
    let o = org_app().await;
    let (_, body) = add_agent(&o.app, Some("tok-ops"), "default", "w2").await;
    let token = body["token"].as_str().unwrap().to_owned();
    assert_eq!(next_as(&o.app, &token).await, StatusCode::OK);
    let (status, body) = revoke_agent(&o.app, Some("tok-ops"), "default", "w2").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["revoked"].as_bool(), body["released"].clone()),
        (Some(true), json!([]))
    );
    assert_eq!(next_as(&o.app, &token).await, StatusCode::UNAUTHORIZED);
    // A retry writes nothing.
    let before = o.org.published().len();
    let (status, body) = revoke_agent(&o.app, Some("tok-ops"), "default", "w2").await;
    assert_eq!(
        (status, body["revoked"].as_bool()),
        (StatusCode::OK, Some(false))
    );
    assert_eq!(o.org.published().len(), before);
    let (status, body) = revoke_agent(&o.app, Some("tok-ops"), "default", "w1").await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::CONFLICT, Some("in_token_file"))
    );
    let (status, body) = revoke_agent(&o.app, Some("tok-ops"), "default", "w9").await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::NOT_FOUND, Some("unknown_agent"))
    );
    let (_, body) = get_json(&o.app, "/v1/teams/default/agents", Some("tok-ops")).await;
    assert_eq!(body["agents"][0]["revoked"], true);
}

#[tokio::test(start_paused = true)]
async fn a_revoke_written_while_the_org_lease_was_away_holds_after_the_retake() {
    let o = org_app().await;
    let (_, body) = add_agent(&o.app, Some("tok-ops"), "default", "w2").await;
    let token = body["token"].as_str().unwrap().to_owned();
    let teams = o.teams.clone();
    let _maintain = tokio::spawn(async move { teams.maintain().await });
    o.org.lease_lost.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(7)).await;
    // Without the lease the view may miss a revoke, so only file tokens serve.
    let (status, body) = call(
        &o.app,
        "/v1/work/next",
        Some(&token),
        json!({ "wait_seconds": 0 }),
    )
    .await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("org_unavailable"))
    );
    assert_eq!(next_as(&o.app, "tok-w1").await, StatusCode::OK);
    // Another tracker holds the lease meanwhile and revokes w2.
    let revoke = OrgLogged::new(
        0,
        Key::new("ops").unwrap(),
        OrgRecord::AgentRevoked {
            team: name("default"),
            agent: Key::new("w2").unwrap(),
        },
    );
    o.org
        .publish(kbl_core::org::ORG_EVENTS, revoke.encode())
        .await
        .unwrap();
    o.org.lease_lost.store(false, Ordering::SeqCst);
    // The other tracker's entry stands for one ttl; the retake waits it out.
    tokio::time::sleep(Duration::from_secs(20)).await;
    assert_eq!(next_as(&o.app, &token).await, StatusCode::UNAUTHORIZED);
    assert_eq!(next_as(&o.app, "tok-w1").await, StatusCode::OK);
}

#[tokio::test]
async fn a_minted_agent_the_token_file_also_names_can_still_be_revoked() {
    let o = org_app().await;
    let org = &o.teams.org().unwrap().org;
    let minted = OrgRecord::AgentAdded {
        team: name("default"),
        agent: Key::new("w1").unwrap(),
        token_sha256: TokenHash::of("tok-minted-w1"),
    };
    org.append(Key::new("ops").unwrap(), minted).await.unwrap();
    assert_eq!(next_as(&o.app, "tok-minted-w1").await, StatusCode::OK);
    let (status, body) = revoke_agent(&o.app, Some("tok-ops"), "default", "w1").await;
    assert_eq!(
        (status, body["revoked"].as_bool()),
        (StatusCode::OK, Some(true)),
        "{body}"
    );
    assert_eq!(
        next_as(&o.app, "tok-minted-w1").await,
        StatusCode::UNAUTHORIZED
    );
    // The file's own w1 keeps working and keeps its holds.
    assert_eq!(next_as(&o.app, "tok-w1").await, StatusCode::OK);
}

#[tokio::test]
async fn a_failed_org_publish_does_not_turn_minted_tokens_away_until_the_next_renewal() {
    let o = org_app().await;
    let (_, body) = add_agent(&o.app, Some("tok-ops"), "default", "w2").await;
    let token = body["token"].as_str().unwrap().to_owned();
    assert_eq!(next_as(&o.app, &token).await, StatusCode::OK);
    o.org.fail_publish.store(true, Ordering::SeqCst);
    let (status, _) = add_agent(&o.app, Some("tok-ops"), "default", "w3").await;
    assert!(!status.is_success(), "{status}");
    o.org.fail_publish.store(false, Ordering::SeqCst);
    // No renewal ran: the failed append itself read the log again.
    assert_eq!(next_as(&o.app, &token).await, StatusCode::OK);
}

#[tokio::test]
async fn a_name_revoked_minted_then_put_in_the_token_file_lists_once() {
    let o = org_app().await;
    let org = &o.teams.org().unwrap().org;
    let by = Key::new("ops").unwrap();
    let agent = Key::new("w1").unwrap();
    let added = OrgRecord::AgentAdded {
        team: name("default"),
        agent: agent.clone(),
        token_sha256: TokenHash::of("tok-minted-w1"),
    };
    org.append(by.clone(), added).await.unwrap();
    let revoked = OrgRecord::AgentRevoked {
        team: name("default"),
        agent,
    };
    org.append(by, revoked).await.unwrap();
    let list = Request::get("/v1/teams/default/agents");
    let (_, body) = send(&o.app, list, Some("tok-ops"), Value::Null).await;
    let rows: Vec<&Value> = body["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["agent"] == "w1")
        .collect();
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["token_file"], true);
    assert_eq!(rows[0]["revoked_minted"], true);
}

#[tokio::test(start_paused = true)]
async fn the_team_list_says_whether_an_org_write_would_be_let_in() {
    let o = org_app().await;
    let (_, body) = get_json(&o.app, "/v1/org/teams", Some("tok-ops")).await;
    assert_eq!(body["org_writable"], true, "{body}");
    o.org.lease_lost.store(true, Ordering::SeqCst);
    let teams = o.teams.clone();
    let _maintain = tokio::spawn(async move { teams.maintain().await });
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (_, body) = get_json(&o.app, "/v1/org/teams", Some("tok-ops")).await;
    assert_eq!(body["org_writable"], false, "{body}");
    let (status, body) = create(&o.app, Some("tok-ops"), "alpha").await;
    assert_eq!(
        (status, why(&body)),
        (StatusCode::SERVICE_UNAVAILABLE, Some("org_unavailable"))
    );
}

#[tokio::test(start_paused = true)]
async fn the_org_lease_is_renewed_as_soon_as_serving_starts() {
    let ttl = Duration::from_secs(3);
    // Starting the teams took most of the lease, or all of it.
    for took in [Duration::from_secs(2), Duration::from_secs(4)] {
        let broker = Quiet::default();
        broker.lease_expires.store(true, Ordering::SeqCst);
        let org = Arc::new(Org::start(broker, ttl, Duration::ZERO).await.unwrap());
        tokio::time::advance(took).await;
        assert!(!org.writable());
        let renewing = org.clone();
        let maintain = tokio::spawn(async move { renewing.maintain().await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            org.writable(),
            "not writable right after serving began, {took:?} in"
        );
        maintain.abort();
    }
}

#[tokio::test(start_paused = true)]
async fn an_org_lease_our_view_says_lapsed_is_renewed_while_felix_still_has_it() {
    let ttl = Duration::from_secs(3);
    let broker = Quiet::default();
    broker.lease_expires.store(true, Ordering::SeqCst);
    *broker.lease_lag.lock().unwrap() = Duration::from_millis(200);
    let org = Arc::new(Org::start(broker, ttl, Duration::ZERO).await.unwrap());
    // Past the tracker's view of the lease, inside Felix's.
    tokio::time::advance(ttl + Duration::from_millis(100)).await;
    let renewing = org.clone();
    let maintain = tokio::spawn(async move { renewing.maintain().await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(org.writable(), "the retake was refused by our own entry");
    maintain.abort();
}

fn team_created(team: &str) -> OrgRecord {
    OrgRecord::TeamCreated {
        team: name(team),
        display_name: None,
        adopted: false,
    }
}

#[tokio::test(start_paused = true)]
async fn a_stale_org_writer_stays_out_after_felix_refuses_it() {
    let broker = Quiet::default();
    broker.keeps.store(true, Ordering::SeqCst);
    broker.fences.store(true, Ordering::SeqCst);
    let stale = Org::start(broker.clone(), Duration::from_secs(15), Duration::ZERO)
        .await
        .unwrap();
    // While this tracker was paused, another took the lease and created beta.
    broker.lease_lost.store(true, Ordering::SeqCst);
    let theirs = OrgLogged::new(0, Key::new("ops").unwrap(), team_created("beta"));
    broker.publish(ORG_EVENTS, theirs.encode()).await.unwrap();
    // Alpha is still new after a reread, so only the lease check stops it.
    for _ in 0..2 {
        assert!(matches!(
            stale
                .append(Key::new("ops").unwrap(), team_created("alpha"))
                .await,
            Err(OrgError::Unavailable(_))
        ));
    }
    assert_eq!(
        broker.log.lock().unwrap().len(),
        1,
        "a tracker without the lease appended after the new holder's record"
    );
}

#[tokio::test(start_paused = true)]
async fn an_org_retry_does_not_skip_another_writers_record_in_flight() {
    let broker = Quiet::default();
    broker.keeps.store(true, Ordering::SeqCst);
    broker.fences.store(true, Ordering::SeqCst);
    let org = Org::start(broker.clone(), Duration::from_secs(15), Duration::ZERO)
        .await
        .unwrap();
    // Offset 0 is given out to a record that has not committed.
    broker.in_flight.store(1, Ordering::SeqCst);
    assert!(matches!(
        org.append(Key::new("ops").unwrap(), team_created("alpha"))
            .await,
        Err(OrgError::Unavailable(_))
    ));
    assert!(
        broker.log.lock().unwrap().is_empty(),
        "alpha skipped the offset in flight"
    );
    // It commits, and the next write lands after it.
    broker.in_flight.store(0, Ordering::SeqCst);
    let theirs = OrgLogged::new(0, Key::new("ops").unwrap(), team_created("beta"));
    broker.publish(ORG_EVENTS, theirs.encode()).await.unwrap();
    let (applied, offset) = org
        .append(Key::new("ops").unwrap(), team_created("alpha"))
        .await
        .unwrap();
    assert_eq!((applied, offset), (Applied::Changed, Some(1)));
}

/// The org log as written, by team created.
fn org_teams(broker: &Quiet) -> Vec<String> {
    broker
        .log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(_, p)| match OrgLogged::decode(p).unwrap().record {
            OrgRecord::TeamCreated { team, .. } => Some(team.to_string()),
            _ => None,
        })
        .collect()
}

/// The reread takes most of the 15 s lease, or this tracker's clock stood
/// still while it ran (a paused process, a sleeping laptop).
const PAUSES: [Duration; 2] = [Duration::from_secs(9), Duration::ZERO];

#[tokio::test(start_paused = true)]
async fn an_org_retry_checks_the_lease_after_it_reads_the_log() {
    for pause in PAUSES {
        let broker = Quiet::default();
        broker.keeps.store(true, Ordering::SeqCst);
        broker.fences.store(true, Ordering::SeqCst);
        let org = Org::start(broker.clone(), Duration::from_secs(15), Duration::ZERO)
            .await
            .unwrap();
        // A record this tracker did not write, with the lease still its own,
        // so the first try is refused and the lease would be confirmed.
        let beta = OrgLogged::new(0, Key::new("ops").unwrap(), team_created("beta"));
        broker.publish(ORG_EVENTS, beta.encode()).await.unwrap();
        let gamma = OrgLogged::new(0, Key::new("ops").unwrap(), team_created("gamma"));
        *broker.slow_read.lock().unwrap() = Some((pause, Some(gamma.encode())));
        let r = org
            .append(Key::new("ops").unwrap(), team_created("alpha"))
            .await;
        assert!(
            matches!(r, Err(OrgError::Unavailable(_))),
            "{pause:?}: {r:?}"
        );
        assert_eq!(org_teams(&broker), ["beta", "gamma"], "{pause:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn an_org_write_after_a_failed_one_checks_the_lease_after_it_reads_the_log() {
    for pause in PAUSES {
        let broker = Quiet::default();
        broker.keeps.store(true, Ordering::SeqCst);
        broker.fences.store(true, Ordering::SeqCst);
        let org = Org::start(broker.clone(), Duration::from_secs(15), Duration::ZERO)
            .await
            .unwrap();
        // The write's answer is lost and so is the read after it, so the next
        // write starts stale with the lease unconfirmed.
        broker.fail_publish.store(true, Ordering::SeqCst);
        broker.fail_next_read.store(true, Ordering::SeqCst);
        assert!(matches!(
            org.append(Key::new("ops").unwrap(), team_created("alpha"))
                .await,
            Err(OrgError::Felix(_))
        ));
        broker.fail_publish.store(false, Ordering::SeqCst);
        let gamma = OrgLogged::new(0, Key::new("ops").unwrap(), team_created("gamma"));
        *broker.slow_read.lock().unwrap() = Some((pause, Some(gamma.encode())));
        let r = org
            .append(Key::new("ops").unwrap(), team_created("alpha"))
            .await;
        assert!(
            matches!(r, Err(OrgError::Unavailable(_))),
            "{pause:?}: {r:?}"
        );
        assert_eq!(org_teams(&broker), ["gamma"], "{pause:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn an_org_write_gives_up_once_the_lease_it_started_on_runs_short() {
    let broker = Quiet::default();
    broker.keeps.store(true, Ordering::SeqCst);
    broker.fences.store(true, Ordering::SeqCst);
    let org = Org::start(broker.clone(), Duration::from_secs(15), Duration::ZERO)
        .await
        .unwrap();
    let beta = OrgLogged::new(0, Key::new("ops").unwrap(), team_created("beta"));
    broker.publish(ORG_EVENTS, beta.encode()).await.unwrap();
    // The reread after the refusal takes 9 s of the 15 s lease. The renewal
    // after it succeeds, but the lease that let the write in is nearly gone,
    // and the core lock has kept renew() out that long.
    *broker.slow_read.lock().unwrap() = Some((Duration::from_secs(9), None));
    let r = org
        .append(Key::new("ops").unwrap(), team_created("alpha"))
        .await;
    assert!(matches!(r, Err(OrgError::Unavailable(_))), "{r:?}");
    assert_eq!(org_teams(&broker), ["beta"]);
}

#[tokio::test(start_paused = true)]
async fn a_retaken_org_lease_is_confirmed_after_the_read() {
    for pause in PAUSES {
        let broker = Quiet::default();
        broker.keeps.store(true, Ordering::SeqCst);
        broker.fences.store(true, Ordering::SeqCst);
        broker.lease_expires.store(true, Ordering::SeqCst);
        let org = Arc::new(
            Org::start(broker.clone(), Duration::from_secs(15), Duration::ZERO)
                .await
                .unwrap(),
        );
        // Confirmed once, so only the retake below leaves the lease unconfirmed.
        assert!(
            org.append(Key::new("ops").unwrap(), team_created("beta"))
                .await
                .is_ok()
        );
        // The lease lapses, and the retake's read is where another tracker
        // takes it and writes.
        tokio::time::advance(Duration::from_secs(16)).await;
        let gamma = OrgLogged::new(0, Key::new("ops").unwrap(), team_created("gamma"));
        *broker.slow_read.lock().unwrap() = Some((pause, Some(gamma.encode())));
        let maintain = tokio::spawn({
            let org = org.clone();
            async move { org.maintain().await }
        });
        for _ in 0..100 {
            if broker.slow_read.lock().unwrap().is_none() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(broker.slow_read.lock().unwrap().is_none(), "no retake");
        // The read holds the core lock, so this returns once it is done.
        let _ = org.state().await;
        let r = org
            .append(Key::new("ops").unwrap(), team_created("alpha"))
            .await;
        maintain.abort();
        assert!(
            matches!(r, Err(OrgError::Unavailable(_))),
            "{pause:?}: {r:?}"
        );
        assert_eq!(org_teams(&broker), ["beta", "gamma"], "{pause:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn an_org_renewal_that_landed_with_its_reply_lost_keeps_the_lease() {
    let ttl = Duration::from_secs(15);
    let broker = Quiet::default();
    broker.lease_expires.store(true, Ordering::SeqCst);
    let org = Arc::new(
        Org::start(broker.clone(), ttl, Duration::ZERO)
            .await
            .unwrap(),
    );
    let renewing = org.clone();
    let maintain = tokio::spawn(async move { renewing.maintain().await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    broker.lose_lease_reply.store(true, Ordering::SeqCst);
    // One round loses its reply, the next renews by a version Felix moved past.
    tokio::time::sleep(ttl / 3 + Duration::from_millis(100)).await;
    assert!(!broker.lose_lease_reply.load(Ordering::SeqCst));
    tokio::time::sleep(ttl / 3).await;
    assert!(org.writable(), "the holder dropped a lease it holds");
    maintain.abort();
}

#[tokio::test(start_paused = true)]
async fn an_org_lease_check_that_landed_with_its_reply_lost_lets_the_write_in() {
    let broker = Quiet::default();
    broker.keeps.store(true, Ordering::SeqCst);
    let org = Org::start(broker.clone(), Duration::from_secs(15), Duration::ZERO)
        .await
        .unwrap();
    // A failed write leaves the lease to be checked by the next one.
    broker.fail_publish.store(true, Ordering::SeqCst);
    let ops = || Key::new("ops").unwrap();
    assert!(org.append(ops(), team_created("alpha")).await.is_err());
    broker.fail_publish.store(false, Ordering::SeqCst);
    broker.lose_lease_reply.store(true, Ordering::SeqCst);
    org.append(ops(), team_created("alpha"))
        .await
        .expect("the write was refused on a lease this tracker holds");
}

#[tokio::test(start_paused = true)]
async fn an_org_lease_taken_at_start_whose_reply_was_lost_is_held() {
    let ttl = Duration::from_secs(15);
    let broker = Quiet::default();
    broker.lose_lease_reply.store(true, Ordering::SeqCst);
    let org = Arc::new(
        Org::start(broker, ttl, Duration::ZERO)
            .await
            .expect("the start gave up on a lease it holds"),
    );
    let renewing = org.clone();
    let maintain = tokio::spawn(async move { renewing.maintain().await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(org.writable());
    maintain.abort();
}

#[tokio::test(start_paused = true)]
async fn an_org_retake_that_landed_with_its_reply_lost_is_held() {
    let ttl = Duration::from_secs(3);
    let broker = Quiet::default();
    broker.keeps.store(true, Ordering::SeqCst);
    broker.lease_expires.store(true, Ordering::SeqCst);
    let org = Arc::new(
        Org::start(broker.clone(), ttl, Duration::ZERO)
            .await
            .unwrap(),
    );
    // The lease ran out; meanwhile another tracker held it and created beta.
    tokio::time::advance(ttl + Duration::from_secs(1)).await;
    let theirs = OrgLogged::new(0, Key::new("ops").unwrap(), team_created("beta"));
    broker.publish(ORG_EVENTS, theirs.encode()).await.unwrap();
    broker.lose_lease_reply.store(true, Ordering::SeqCst);
    let renewing = org.clone();
    let maintain = tokio::spawn(async move { renewing.maintain().await });
    // Well inside the entry's time to live, which our own retake started.
    tokio::time::sleep(ttl / 2).await;
    assert!(org.writable(), "the retake was refused by our own entry");
    maintain.abort();
    // The retake read what the other tracker wrote.
    assert!(matches!(
        org.append(Key::new("ops").unwrap(), team_created("beta"))
            .await,
        Ok((Applied::Unchanged, None))
    ));
}

#[tokio::test(start_paused = true)]
async fn an_org_retake_adopted_a_round_late_reads_what_another_wrote() {
    let ttl = Duration::from_secs(3);
    let broker = Quiet::default();
    broker.keeps.store(true, Ordering::SeqCst);
    broker.lease_expires.store(true, Ordering::SeqCst);
    let org = Arc::new(
        Org::start(broker.clone(), ttl, Duration::ZERO)
            .await
            .unwrap(),
    );
    tokio::time::advance(ttl + Duration::from_secs(1)).await;
    let theirs = OrgLogged::new(0, Key::new("ops").unwrap(), team_created("beta"));
    broker.publish(ORG_EVENTS, theirs.encode()).await.unwrap();
    // The retake lands with its reply lost, and the lease key cannot be read.
    broker.lose_lease_reply.store(true, Ordering::SeqCst);
    broker.fail_lease_get.store(true, Ordering::SeqCst);
    let renewing = org.clone();
    let maintain = tokio::spawn(async move { renewing.maintain().await });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!broker.lose_lease_reply.load(Ordering::SeqCst));
    broker.fail_lease_get.store(false, Ordering::SeqCst);
    // The next round finds the entry ours at a version it never heard of.
    tokio::time::sleep(ttl / 3).await;
    assert!(org.writable(), "the retake was refused by our own entry");
    maintain.abort();
    assert!(matches!(
        org.append(Key::new("ops").unwrap(), team_created("beta"))
            .await,
        Ok((Applied::Unchanged, None))
    ));
}
