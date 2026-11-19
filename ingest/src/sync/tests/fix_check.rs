//! A red check that the flake re-run leaves red is reported as `check_failed`
//! with the end of its job log, once per request, check and head commit.

use kbl_core::names::Key;
use kbl_core::work::FORMAT_V23;

use super::*;

const LOG: &str = "/actions/jobs/7001/logs";

fn job(name: &str, conclusion: &str, run: u64) -> Value {
    json!({
        "name": name,
        "status": "completed",
        "conclusion": conclusion,
        "html_url": format!("https://github.com/GetFelix/felix/actions/runs/{run}/job/{run}1"),
        "output": { "title": "failed", "summary": "the runner timed out", "text": null },
    })
}

fn red(rig: &Rig, run: u64) {
    rig.pr(pull(|_| {}), vec![], vec![job("lint", "failure", run)]);
}

fn log_reads(rig: &Rig) -> usize {
    rig.gh
        .sent()
        .iter()
        .filter(|r| r.url.contains("/logs"))
        .count()
}

/// The `log` of every `check_failed` the tracker took.
fn logs(rig: &Rig) -> Vec<String> {
    rig.tracker
        .records()
        .into_iter()
        .filter_map(|r| match r {
            WorkRecord::CheckFailed { log, .. } => Some(log),
            _ => None,
        })
        .collect()
}

fn failures(rig: &Rig) -> Vec<(String, String)> {
    rig.tracker
        .state()
        .items
        .values()
        .flat_map(|it| it.check_failures.iter())
        .map(|f| (f.check.clone(), f.head.clone()))
        .collect()
}

#[tokio::test]
async fn a_red_check_is_reported_once_with_the_end_of_its_log() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    rig.tracker
        .person(WorkRecord::FormatRaised { to: FORMAT_V23 });
    let full = format!("{}error: unused import", "x".repeat(10_000));
    let mut log = status(200, &[("content-type", "text/plain")]);
    log.body = Value::String(full.clone());
    rig.gh.on("GET", LOG, log);

    red(&rig, 700);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("check_failed"), 1);
    let kinds = rig.tracker.kinds();
    let at = |k| kinds.iter().rposition(|x| *x == k).unwrap();
    assert!(at("pr_status") < at("check_failed"), "{kinds:?}");
    let s = rig.tracker.state();
    let f = &s.items[&item].check_failures[0];
    assert_eq!(f.check, "lint");
    assert!(!f.head.is_empty());
    assert_eq!(log_reads(&rig), 1);
    let excerpt = &logs(&rig)[0];
    assert!(excerpt.len() <= 4096, "{}", excerpt.len());
    assert!(full.ends_with(excerpt.as_str()));
    assert!(excerpt.ends_with("error: unused import"));

    // The same red on the next poll is neither fetched nor reported again.
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("check_failed"), 1);
    assert_eq!(log_reads(&rig), 1);
}

#[tokio::test]
async fn a_pending_flake_rerun_reports_nothing_until_it_fails_again() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    rig.tracker
        .person(WorkRecord::FormatRaised { to: FORMAT_V23 });
    rig.tracker.person(WorkRecord::FlakeSet {
        flake: Key::new("lint-timeout").unwrap(),
        check: "lint".into(),
        pattern: Some("timed out".into()),
        issue: "GetFelix/felix#9".into(),
    });
    rig.gh.on(
        "POST",
        "/actions/runs/700/rerun-failed-jobs",
        status(201, &[]),
    );

    red(&rig, 700);
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("check_failed"), 0);

    // The re-run failed too: now it is reported, with the check's own output
    // because this job's log is not readable.
    red(&rig, 701);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("check_failed"), 1);
    assert_eq!(failures(&rig).len(), 1);
}

#[tokio::test]
async fn the_job_log_is_read_through_githubs_redirect_without_the_token() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    rig.tracker
        .person(WorkRecord::FormatRaised { to: FORMAT_V23 });
    let blob = "https://results.blob.core.windows.net/logs/7001.txt?sig=abc";
    rig.gh.on("GET", LOG, status(302, &[("location", blob)]));
    // GitHub serves the log as bytes, not as text/plain.
    let mut body = status(200, &[("content-type", "application/octet-stream")]);
    body.body = Value::String("step 1\nerror: unused import".into());
    rig.gh.on("GET", "blob.core.windows.net", body);

    red(&rig, 700);
    ingest.poll(false).await.unwrap();
    assert_eq!(logs(&rig), ["step 1\nerror: unused import"]);
    let sent = rig.gh.sent();
    let api = sent.iter().find(|r| r.url.contains(LOG)).unwrap();
    let signed = sent.iter().find(|r| r.url == blob).unwrap();
    assert!(!api.anon && api.tail == Some(4096));
    assert!(signed.anon, "the signed link must not get the token");
}

#[tokio::test]
async fn a_failure_refused_for_its_format_is_reported_after_the_upgrade() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);

    red(&rig, 700);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("check_failed"), 0);

    rig.tracker
        .person(WorkRecord::FormatRaised { to: FORMAT_V23 });
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("check_failed"), 1);
    assert_eq!(failures(&rig).len(), 1);
}

#[tokio::test]
async fn a_redirect_to_plain_http_is_not_followed() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    rig.tracker
        .person(WorkRecord::FormatRaised { to: FORMAT_V23 });
    let blob = "http://results.blob.core.windows.net/logs/7001.txt?sig=abc";
    rig.gh.on("GET", LOG, status(302, &[("location", blob)]));
    let mut body = status(200, &[]);
    body.body = Value::String("from the plain link".into());
    rig.gh.on("GET", "blob.core.windows.net", body);

    red(&rig, 700);
    ingest.poll(false).await.unwrap();
    assert!(rig.gh.sent().iter().all(|r| r.url != blob));
    // The check's own output stands in for the log.
    assert_eq!(logs(&rig), ["failed\nthe runner timed out\n"]);
}

#[tokio::test]
async fn a_log_read_cut_by_the_fence_is_reported_later_with_the_log() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    rig.tracker
        .person(WorkRecord::FormatRaised { to: FORMAT_V23 });
    // What the lease gate answers when the fence closes mid-request.
    rig.gh.on("GET", LOG, cut());
    red(&rig, 700);
    assert!(ingest.poll(false).await.is_err());
    assert_eq!(rig.tracker.count("check_failed"), 0);

    let mut log = status(200, &[]);
    log.body = Value::String("error: unused import".into());
    rig.gh.on("GET", LOG, log);
    ingest.poll(false).await.unwrap();
    assert_eq!(logs(&rig), ["error: unused import"]);
}

#[tokio::test]
async fn a_failure_refused_at_the_cap_is_not_read_again() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    rig.tracker
        .person(WorkRecord::FormatRaised { to: FORMAT_V23 });
    let mut log = status(200, &[]);
    log.body = Value::String("error: unused import".into());
    rig.gh.on("GET", LOG, log);
    red(&rig, 700);
    ingest.poll(false).await.unwrap();
    assert_eq!(log_reads(&rig), 1);
    let s = rig.tracker.state();
    let (pr, url) = {
        let st = s.items[&item].pr_status.as_ref().unwrap();
        (st.pr.clone(), st.failing.as_ref().unwrap().url.clone())
    };
    // The root's failures fill up to the cap.
    for n in 1..32 {
        rig.tracker.tracker(WorkRecord::CheckFailed {
            item: item.clone(),
            pr: pr.clone(),
            check: "lint".into(),
            url: url.clone(),
            head: format!("h{n}"),
            log: String::new(),
            fix: false,
        });
    }

    // A push fails lint again: refused at the cap, read once, not on every poll.
    rig.pr(
        pull(|v| v["head"]["sha"] = json!("def456")),
        vec![],
        vec![job("lint", "failure", 700)],
    );
    ingest.poll(false).await.unwrap();
    assert_eq!(log_reads(&rig), 2);
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();
    assert_eq!(log_reads(&rig), 2);
    assert_eq!(rig.tracker.count("check_failed"), 32);
}

#[test]
fn the_excerpt_is_the_end_of_the_log_on_a_character_boundary() {
    assert_eq!(crate::sync::tail("abcdef", 3), "def");
    assert_eq!(crate::sync::tail("aé", 1), "");
    assert_eq!(crate::sync::tail("ab", 10), "ab");
}
