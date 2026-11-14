//! The org routes, and a team that is not serving while the others are.

use std::collections::HashMap;

use kbl_core::org::OrgRecord;
use kbl_core::work::WORK_EVENTS;

use super::*;
use crate::org::Org;
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
    assert!(
        o.org.published().is_empty(),
        "a refused create wrote to org.events"
    );

    let (status, body) = create(&o.app, Some("tok-ops"), "alpha").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["created"].as_bool(), body["serving"].as_bool()),
        (Some(true), Some(true))
    );
    assert_eq!(o.org.published(), vec!["org.events".to_owned()]);
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
    assert_eq!(o.org.published().len(), 1);

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
