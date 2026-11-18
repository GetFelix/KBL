//! `PUT /v1/ingest/leases`: who gets a source, when it moves, and its epoch.

use kbl_core::lease::{LEASE_MS, Lease, TAKE_MARGIN_MS};

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
    let (status, body) = beat_raw(app, group, instance, leave, &[]).await;
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

async fn beat_raw(
    app: &Router,
    group: &str,
    instance: &str,
    leave: bool,
    held: &[&str],
) -> (StatusCode, Value) {
    let body = json!({ "group": group, "instance": instance, "leave": leave, "held": held });
    send(app, Request::put("/v1/ingest/leases"), Some("tok-i1"), body).await
}

/// A second tracker over the same Felix, with its own memory and clock.
fn other_tracker(o: &OrgApp) -> (Router, Arc<Groups>) {
    let groups = Arc::new(Groups::default());
    (
        crate::http::router_with(o.teams.clone(), groups.clone()),
        groups,
    )
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
    // At a's expiry the take margin still holds it.
    advance(&groups, LEASE_MS - 30_000);
    assert!(beat(&app, "b").await.1.is_empty(), "inside the take margin");
    // Past the 90 s from a's first take and the margin, b takes its share at
    // the next epoch.
    advance(&groups, TAKE_MARGIN_MS);
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
    advance(&groups, LEASE_MS + TAKE_MARGIN_MS);
    let (_, b_got) = beat(&app, "b").await;
    assert_eq!(names(&b_got), SOURCES);
}

#[tokio::test]
async fn a_leaving_worker_releases_on_any_tracker_and_the_next_take_bumps_the_epoch() {
    let (o, app, groups) = leased().await;
    beat(&app, "a").await;
    // The leave reaches a tracker that never served a's heartbeats.
    let (other, other_groups) = other_tracker(&o);
    let (status, _) = beat_in(&other, "g1", "a", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stored(&o, "github:o/a").holder, None);
    assert!(beat(&app, "b").await.1.is_empty(), "inside the take margin");
    advance(&groups, TAKE_MARGIN_MS);
    advance(&other_groups, TAKE_MARGIN_MS);
    let (_, b_got) = beat(&app, "b").await;
    assert_eq!(names(&b_got), SOURCES);
    assert!(b_got.iter().all(|(_, e)| *e == 2));
}

#[tokio::test]
async fn trackers_agree_on_the_assignment_whichever_one_a_worker_reaches() {
    let (o, app, groups) = leased().await;
    let (other, other_groups) = other_tracker(&o);
    beat(&app, "a").await;
    // b is only ever heard by the other tracker.
    beat(&other, "b").await;
    advance(&groups, 30_000);
    advance(&other_groups, 30_000);
    let (_, a_got) = beat(&app, "a").await;
    assert!(
        !a_got.is_empty() && a_got.len() < SOURCES.len(),
        "a's tracker knows b is live and renews only a's share: {a_got:?}"
    );
    // Both trackers give a the same share, so its leases do not flap.
    let (_, a_other) = beat(&other, "a").await;
    assert_eq!(a_other, a_got);
}

#[tokio::test]
async fn a_moved_source_stays_in_the_answer_while_its_lease_is_live() {
    let (o, app, groups) = leased().await;
    beat(&app, "a").await;
    beat(&app, "b").await;
    advance(&groups, 30_000);
    let (_, body) = beat_raw(&app, "g1", "a", false, &SOURCES).await;
    let leases = body["leases"].as_array().unwrap();
    assert_eq!(leases.len(), SOURCES.len(), "nothing dropped: {leases:?}");
    let winding: Vec<&Value> = leases.iter().filter(|l| l["until_ms"].is_u64()).collect();
    assert!(!winding.is_empty(), "b's share is winding down");
    for l in winding {
        // Taken at 0 with a 90 s expiry, 30 s ago, and not renewed since.
        let left = l["until_ms"].as_u64().unwrap();
        assert!(
            (LEASE_MS - 31_000..=LEASE_MS - 30_000).contains(&left),
            "{left}"
        );
        let s = l["source"].as_str().unwrap();
        assert_eq!(
            stored(&o, s).holder.as_deref(),
            Some("i1/a"),
            "not renewed or moved"
        );
    }
    // Past its expiry it is no longer answered.
    advance(&groups, LEASE_MS - 30_000);
    beat(&app, "b").await;
    let (_, body) = beat_raw(&app, "g1", "a", false, &SOURCES).await;
    let leases = body["leases"].as_array().unwrap();
    assert!(leases.len() < SOURCES.len());
    assert!(leases.iter().all(|l| l["until_ms"].is_null()));
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

#[tokio::test]
async fn a_heartbeat_that_lands_after_the_leave_does_not_bring_the_worker_back() {
    let (o, app, groups) = leased().await;
    beat(&app, "a").await;
    let (status, _) = beat_in(&app, "g1", "a", true).await;
    assert_eq!(status, StatusCode::OK);
    // A renew a sent before leaving reaches another tracker afterwards.
    let (other, other_groups) = other_tracker(&o);
    advance(&groups, TAKE_MARGIN_MS);
    advance(&other_groups, TAKE_MARGIN_MS);
    let (status, body) = beat_raw(&other, "g1", "a", false, &[]).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["why"], "left");
    assert_eq!(stored(&o, "github:o/a").holder, None);
    // The group goes on without the ghost: b takes everything.
    let (_, b_got) = beat(&app, "b").await;
    assert_eq!(names(&b_got), SOURCES);
}

#[tokio::test]
async fn a_leave_releases_the_leases_even_when_the_membership_write_fails() {
    let (o, app, groups) = leased().await;
    beat(&app, "a").await;
    o.broker("default")
        .fail_members_put
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, _) = beat_in(&app, "g1", "a", true).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(stored(&o, "github:o/a").holder, None, "released anyway");
    o.broker("default")
        .fail_members_put
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let _ = groups;
}

#[tokio::test]
async fn two_hundred_workers_joining_at_once_all_get_in() {
    let (o, app, _groups) = leased().await;
    // The in-process cache has no write contention, so make the first
    // writes lose as a crowd of trackers would.
    o.broker("default")
        .lose_members_puts
        .store(50, std::sync::atomic::Ordering::SeqCst);
    let names: Vec<String> = (0..200).map(|i| format!("w{i}")).collect();
    // Past the team's in-flight cap a call is turned away with a 1 s
    // Retry-After and tried again. Any other refusal is a lost membership
    // write.
    let beats = names.iter().map(|n| {
        let app = &app;
        async move {
            loop {
                let (status, body) = beat_raw(app, "g1", n, false, &[]).await;
                if body["why"] != "team_busy" {
                    return (status, body);
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    });
    let answers = tokio::time::timeout(Duration::from_secs(30), futures::future::join_all(beats))
        .await
        .expect("every worker in well inside the fence");
    let refused: Vec<_> = answers
        .iter()
        .zip(&names)
        .filter(|((s, _), _)| *s != StatusCode::OK)
        .map(|((s, b), n)| format!("{n}: {s} {b}"))
        .collect();
    assert!(refused.is_empty(), "{} refused: {refused:?}", refused.len());
}

#[tokio::test]
async fn a_heartbeat_answers_how_many_sources_the_pool_serves_now() {
    let (o, app, _) = leased().await;
    let (_, body) = beat_raw(&app, "g1", "a", false, &[]).await;
    assert_eq!(body["pool_sources"], json!(SOURCES.len()));
    let mut more = SOURCES.to_vec();
    more.push("github:o/g");
    let rec = OrgRecord::SourceAdded {
        team: name("default"),
        source: "github:o/g".into(),
    };
    o.teams
        .org()
        .unwrap()
        .org
        .append(Key::new("ops").unwrap(), rec)
        .await
        .unwrap();
    let (status, _) = org_record(&o, "tok-ops", connection("default", "g1", &more)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = beat_raw(&app, "g1", "a", false, &[]).await;
    assert_eq!(body["pool_sources"], json!(SOURCES.len() + 1));
}
