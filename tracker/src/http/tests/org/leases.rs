//! `PUT /v1/ingest/leases`: who gets a source, when it moves, and its epoch.

use kbl_core::lease::{LEASE_MS, Lease};

use super::*;
use crate::http::ingest::leases::Groups;

const SOURCES: [&str; 6] = [
    "github:o/a",
    "github:o/b",
    "github:o/c",
    "github:o/d",
    "github:o/e",
    "github:o/f",
];

/// A tracker whose team `default` has a connection for group `g1` over
/// [`SOURCES`], with a lease clock the test moves.
async fn leased() -> (OrgApp, Router, Arc<Groups>) {
    let o = connected().await;
    let ops = Key::new("ops").unwrap();
    for s in SOURCES {
        let rec = OrgRecord::SourceAdded {
            team: name("default"),
            source: s.into(),
        };
        o.teams
            .org()
            .unwrap()
            .org
            .append(ops.clone(), rec)
            .await
            .unwrap();
    }
    let (status, _) = org_record(&o, "tok-ops", connection("default", "g1", &SOURCES)).await;
    assert_eq!(status, StatusCode::OK);
    let groups = Arc::new(Groups::default());
    let app = crate::http::router_with(o.teams.clone(), groups.clone());
    (o, app, groups)
}

async fn beat(app: &Router, instance: &str) -> (StatusCode, Vec<(String, u64)>) {
    beat_in(app, "g1", instance, false).await
}

async fn beat_in(
    app: &Router,
    group: &str,
    instance: &str,
    leave: bool,
) -> (StatusCode, Vec<(String, u64)>) {
    let body = json!({ "group": group, "instance": instance, "leave": leave });
    let (status, body) = send(app, Request::put("/v1/ingest/leases"), Some("tok-i1"), body).await;
    let leases = body["leases"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|l| {
            (
                l["source"].as_str().unwrap().to_owned(),
                l["epoch"].as_u64().unwrap(),
            )
        })
        .collect();
    (status, leases)
}

fn advance(groups: &Groups, ms: u64) {
    groups
        .skew_ms
        .fetch_add(ms, std::sync::atomic::Ordering::SeqCst);
}

fn stored(o: &OrgApp, source: &str) -> Lease {
    let leases = o.broker("default").source_leases.lock().unwrap().clone();
    serde_json::from_slice(&leases[source].0).unwrap()
}

fn names(leases: &[(String, u64)]) -> Vec<&str> {
    leases.iter().map(|(s, _)| s.as_str()).collect()
}

#[tokio::test]
async fn a_lone_worker_takes_every_source_at_epoch_one_and_renews_at_the_same_epoch() {
    let (o, app, groups) = leased().await;
    let (status, got) = beat(&app, "a").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&got), SOURCES);
    assert!(got.iter().all(|(_, e)| *e == 1));
    advance(&groups, 30_000);
    let (_, again) = beat(&app, "a").await;
    assert_eq!(again, got, "a renew keeps the epoch");
    assert_eq!(stored(&o, "github:o/a").holder.as_deref(), Some("i1/a"));
}

#[tokio::test]
async fn a_joining_worker_gets_its_share_only_after_the_old_holder_lets_it_expire() {
    let (o, app, groups) = leased().await;
    beat(&app, "a").await;
    // b joins: its share is still held by a, so it gets nothing yet.
    let (_, b_got) = beat(&app, "b").await;
    assert!(b_got.is_empty(), "a live lease does not move: {b_got:?}");
    // a renews only what is still its share; b's share is left to run out.
    advance(&groups, 30_000);
    let (_, a_got) = beat(&app, "a").await;
    let b_share: Vec<&str> = SOURCES
        .into_iter()
        .filter(|s| !names(&a_got).contains(s))
        .collect();
    assert!(
        !b_share.is_empty() && !a_got.is_empty(),
        "a split: {a_got:?}"
    );
    assert!(
        beat(&app, "b").await.1.is_empty(),
        "still inside the expiry"
    );
    // Past the 90 s from a's first take, b takes its share at the next epoch.
    advance(&groups, LEASE_MS - 30_000);
    let (_, b_got) = beat(&app, "b").await;
    assert_eq!(names(&b_got), b_share);
    assert!(
        b_got.iter().all(|(_, e)| *e == 2),
        "a take bumps: {b_got:?}"
    );
    // The old holder's renew is refused: it gets only its own share back.
    let (_, a_again) = beat(&app, "a").await;
    assert_eq!(names(&a_again), names(&a_got));
    for s in b_share {
        let l = stored(&o, s);
        assert_eq!((l.holder.as_deref(), l.epoch), (Some("i1/b"), 2));
    }
}

#[tokio::test]
async fn a_worker_that_stops_heartbeating_loses_its_sources_to_the_rest() {
    let (_, app, groups) = leased().await;
    beat(&app, "a").await;
    beat(&app, "b").await;
    // a goes quiet. Once it has been unheard for the expiry it leaves the
    // group, and its leases have run out too.
    advance(&groups, LEASE_MS);
    let (_, b_got) = beat(&app, "b").await;
    assert_eq!(names(&b_got), SOURCES);
}

#[tokio::test]
async fn a_leaving_worker_releases_at_once_and_the_next_take_bumps_the_epoch() {
    let (o, app, _) = leased().await;
    beat(&app, "a").await;
    let (status, _) = beat_in(&app, "g1", "a", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stored(&o, "github:o/a").holder, None);
    let (_, b_got) = beat(&app, "b").await;
    assert_eq!(names(&b_got), SOURCES);
    assert!(b_got.iter().all(|(_, e)| *e == 2));
}

#[tokio::test]
async fn a_take_that_loses_the_version_race_is_not_granted() {
    let (o, app, _) = leased().await;
    // Another tracker, with its own view of who is live, takes o/a first.
    let other = Lease {
        holder: Some("i1/z".into()),
        epoch: 1,
        expires_ms: u64::MAX,
    };
    *o.broker("default").race_lease_put.lock().unwrap() = Some(serde_json::to_vec(&other).unwrap());
    let (_, got) = beat(&app, "a").await;
    let lost = SOURCES.len() - got.len();
    assert_eq!(lost, 1, "exactly the raced source is not granted: {got:?}");
    let raced = SOURCES
        .into_iter()
        .find(|s| !names(&got).contains(s))
        .unwrap();
    assert_eq!(stored(&o, raced).holder.as_deref(), Some("i1/z"));
}

#[tokio::test]
async fn a_worker_gets_no_lease_on_another_teams_source() {
    let (o, app, _) = leased().await;
    // `other` names the same group for its own source.
    org_record(&o, "tok-ops", connection("other", "g1", &["github:o/two"])).await;
    let (_, got) = beat(&app, "a").await;
    assert!(!names(&got).contains(&"github:o/two"));
    assert!(o.broker("other").source_leases.lock().unwrap().is_empty());
    // A group no connection of its team names gets nothing.
    let (status, got) = beat_in(&app, "g2", "a", false).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(got.is_empty());
    let (status, _) = send(
        &app,
        Request::put("/v1/ingest/leases"),
        Some("tok-w1"),
        json!({ "group": "g1", "instance": "a" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "an agent token");
}

#[tokio::test]
async fn the_config_answer_lists_the_workers_leases_and_wakes_on_a_heartbeat() {
    let (o, app, _) = leased().await;
    let o = OrgApp { app, ..o };
    let (_, body, etag) = config(&o, "group=g1&instance=a", None).await;
    assert_eq!(body["leases"], json!([]));
    assert_eq!(body["assigned"], json!([]), "not live until it heartbeats");
    let o = Arc::new(o);
    let o2 = o.clone();
    let held =
        tokio::spawn(async move { config(&o2, "group=g1&instance=a&wait=30", Some(&etag)).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    beat(&o.app, "a").await;
    let (status, body, _) = tokio::time::timeout(Duration::from_secs(5), held)
        .await
        .expect("the heartbeat wakes the held poll")
        .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["leases"].as_array().unwrap().len(), SOURCES.len());
    assert_eq!(body["leases"][0]["epoch"], json!(1));
}
