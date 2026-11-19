//! A failed check on the team's flake list is re-run once before it becomes
//! work.

use kbl_core::names::Key;
use kbl_core::work::FORMAT_V22;

use super::*;

const RERUN: &str = "/actions/runs/700/rerun-failed-jobs";

/// A finished job of Actions run `run`, with the summary GitHub shows.
fn job(name: &str, conclusion: &str, run: u64, summary: &str) -> Value {
    json!({
        "name": name,
        "status": "completed",
        "conclusion": conclusion,
        "html_url": format!("https://github.com/GetFelix/felix/actions/runs/{run}/job/{run}1"),
        "output": { "title": "failed", "summary": summary, "text": null },
    })
}

fn flake(rig: &Rig, check: &str, pattern: Option<&str>) {
    rig.tracker
        .person(WorkRecord::FormatRaised { to: FORMAT_V22 });
    rig.tracker.person(WorkRecord::FlakeSet {
        flake: Key::new("lint-timeout").unwrap(),
        check: check.into(),
        pattern: pattern.map(Into::into),
        issue: "GetFelix/felix#9".into(),
    });
}

fn reruns(rig: &Rig) -> usize {
    rig.gh
        .writes()
        .iter()
        .filter(|w| w.contains("rerun-failed-jobs"))
        .count()
}

fn checks(rig: &Rig, item: &kbl_core::names::ItemId) -> PrChecks {
    rig.tracker.state().items[item]
        .pr_status
        .clone()
        .unwrap()
        .checks
}

fn red(rig: &Rig, run: u64, summary: &str) {
    rig.pr(
        pull(|_| {}),
        vec![],
        vec![
            job("unit", "success", 1, ""),
            job("lint", "failure", run, summary),
        ],
    );
}

#[tokio::test]
async fn a_known_flake_is_rerun_and_a_green_rerun_never_becomes_work() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    flake(&rig, "lint", Some("timed out"));
    rig.gh.on("POST", RERUN, status(201, &[]));

    red(&rig, 700, "the runner timed out");
    ingest.poll(false).await.unwrap();
    assert_eq!(reruns(&rig), 1);
    assert_eq!(checks(&rig, &item), PrChecks::Pending);
    assert_ne!(rig.tracker.state().stage(&item), Some(Stage::NeedsYou));

    // The old failure is still what GitHub shows for a moment.
    ingest.poll(false).await.unwrap();
    assert_eq!(reruns(&rig), 1);
    assert_eq!(checks(&rig, &item), PrChecks::Pending);

    rig.pr(
        pull(|_| {}),
        vec![],
        vec![
            job("unit", "success", 1, ""),
            job("lint", "success", 701, ""),
        ],
    );
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    assert_eq!(checks(&rig, &item), PrChecks::Green);
    assert_eq!(s.items[&item].flake_retried.as_ref().unwrap().check, "lint");
    assert_eq!(s.flakes[&Key::new("lint-timeout").unwrap()].hits, 1);
    assert_eq!(rig.tracker.count("pr_status"), 2);
    assert!(rig.tracker.saved_state().reruns.is_empty());
}

#[tokio::test]
async fn a_rerun_that_fails_again_goes_the_normal_way_and_is_not_repeated() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    flake(&rig, "lint", None);
    rig.gh.on("POST", RERUN, status(201, &[]));

    red(&rig, 700, "boom");
    ingest.poll(false).await.unwrap();
    assert_eq!(checks(&rig, &item), PrChecks::Pending);

    // The re-run replaced the check with a new failed job.
    red(&rig, 701, "boom");
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    assert_eq!(checks(&rig, &item), PrChecks::Red);
    assert_eq!(s.needs_you(&item), Some(NeedsYou::CheckFailed));
    assert!(s.items[&item].flake_retried.is_none());
    assert_eq!(s.flakes[&Key::new("lint-timeout").unwrap()].hits, 1);
    assert_eq!(reruns(&rig), 1);
}

#[tokio::test]
async fn a_failure_the_list_does_not_match_is_red_at_once() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    flake(&rig, "lint", Some("timed out"));
    rig.gh.on("POST", RERUN, status(201, &[]));

    red(&rig, 700, "assertion failed");
    ingest.poll(false).await.unwrap();
    assert_eq!(checks(&rig, &item), PrChecks::Red);
    assert_eq!(reruns(&rig), 0);

    // Another check with the same pattern is not the entry's check either.
    rig.pr(
        pull(|_| {}),
        vec![],
        vec![job("unit", "failure", 700, "timed out")],
    );
    ingest.poll(false).await.unwrap();
    assert_eq!(reruns(&rig), 0);
}

#[tokio::test]
async fn a_refused_rerun_is_red_and_not_asked_again() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    flake(&rig, "lint", None);
    rig.gh.on("POST", RERUN, status(403, &[]));

    red(&rig, 700, "boom");
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();
    assert_eq!(checks(&rig, &item), PrChecks::Red);
    assert_eq!(reruns(&rig), 1);
}

#[tokio::test]
async fn a_new_commit_gets_its_own_rerun() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    flake(&rig, "lint", None);
    rig.gh.on("POST", RERUN, status(201, &[]));
    rig.gh.on(
        "POST",
        "/actions/runs/800/rerun-failed-jobs",
        status(201, &[]),
    );

    red(&rig, 700, "boom");
    ingest.poll(false).await.unwrap();
    red(&rig, 701, "boom");
    ingest.poll(false).await.unwrap();
    assert_eq!(checks(&rig, &item), PrChecks::Red);

    rig.pr(
        pull(|p| p["head"]["sha"] = json!("def456")),
        vec![],
        vec![job("lint", "failure", 800, "boom")],
    );
    ingest.poll(false).await.unwrap();
    assert_eq!(reruns(&rig), 2);
    assert_eq!(checks(&rig, &item), PrChecks::Pending);
}

#[tokio::test]
async fn a_rerun_that_never_shows_is_given_up_on() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    flake(&rig, "lint", None);
    rig.gh.on("POST", RERUN, status(201, &[]));

    red(&rig, 700, "boom");
    for _ in 0..=MAX_RERUN_WAITS + 1 {
        ingest.poll(false).await.unwrap();
    }
    assert_eq!(checks(&rig, &item), PrChecks::Red);
    assert_eq!(reruns(&rig), 1);
}

#[tokio::test]
async fn another_check_going_red_is_not_the_reruns_outcome() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    flake(&rig, "lint", None);
    rig.gh.on("POST", RERUN, status(201, &[]));

    red(&rig, 700, "boom");
    ingest.poll(false).await.unwrap();

    // Unit fails on the same head while the lint re-run is still out.
    rig.pr(
        pull(|_| {}),
        vec![],
        vec![job("unit", "failure", 900, "other")],
    );
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    assert!(s.items[&item].flake_retried.is_none());
    assert_eq!(checks(&rig, &item), PrChecks::Pending);
    assert!(!rig.tracker.saved_state().reruns.values().any(|r| r.spent));
}

#[tokio::test]
async fn a_spent_rerun_stays_spent_through_a_pending_poll() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    flake(&rig, "lint", None);
    rig.gh.on("POST", RERUN, status(201, &[]));

    red(&rig, 700, "boom");
    ingest.poll(false).await.unwrap();
    red(&rig, 701, "boom");
    ingest.poll(false).await.unwrap();
    assert_eq!(checks(&rig, &item), PrChecks::Red);

    // Someone re-runs it by hand: pending, then red again on the same head.
    rig.pr(
        pull(|_| {}),
        vec![],
        vec![{
            let mut j = job("lint", "success", 702, "");
            j["status"] = json!("in_progress");
            j["conclusion"] = Value::Null;
            j
        }],
    );
    ingest.poll(false).await.unwrap();
    red(&rig, 702, "boom");
    ingest.poll(false).await.unwrap();
    assert_eq!(reruns(&rig), 1);
    assert_eq!(checks(&rig, &item), PrChecks::Red);
}

#[tokio::test]
async fn a_pruned_item_takes_its_rerun_entry_with_it() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    flake(&rig, "lint", None);
    rig.gh.on("POST", RERUN, status(201, &[]));

    red(&rig, 700, "boom");
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.saved_state().reruns.len(), 1);

    rig.pr(
        pull(|p| {
            p["state"] = json!("closed");
            p["merged"] = json!(true);
        }),
        vec![],
        vec![],
    );
    rig.list(vec![closed(
        issue(812, |_| {}),
        "completed",
        "2026-11-20T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();
    let saved = rig.tracker.saved_state();
    assert!(saved.items.is_empty());
    assert!(saved.reruns.is_empty());
}
