use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::*;
use crate::testing::{Script, fixture, issue, ok, status};

const REPOS: &[&str] = &["GetFelix/felix"];

fn github(gh: &Script) -> Github<Script> {
    Github::new("https://api.github.com", gh.clone(), vec![])
}

fn repos() -> Vec<String> {
    REPOS.iter().map(|s| (*s).to_owned()).collect()
}

fn none() -> BTreeMap<String, Cursor> {
    BTreeMap::new()
}

/// The cursor of the one repo in `REPOS`.
fn cur(c: &Changes) -> &Cursor {
    &c.cursors["GetFelix/felix"]
}

fn at_repo(c: Cursor) -> BTreeMap<String, Cursor> {
    BTreeMap::from([("GetFelix/felix".to_owned(), c)])
}

fn with(mut res: Res, headers: &[(&str, &str)]) -> Res {
    for (k, v) in headers {
        res.headers.insert((*k).to_owned(), (*v).to_owned());
    }
    res
}

#[tokio::test]
async fn a_listing_maps_fields_and_leaves_out_pull_requests() {
    let gh = Script::default();
    let mut pr = issue(900, |_| {});
    pr["pull_request"] = json!({ "url": "x" });
    let mut closed = issue(813, |v| {
        v["state"] = json!("closed");
        v["state_reason"] = json!("not_planned");
        v["milestone"] = json!({ "title": "0.8" });
        v["assignees"] = json!([{ "login": "carol" }]);
    });
    closed["updated_at"] = json!("2026-11-10T12:00:00Z");
    gh.on(
        "GET",
        "/issues?",
        ok(json!([issue(812, |_| {}), pr, closed])),
    );
    let changes = github(&gh).changed(&repos(), &none()).await.unwrap();
    assert_eq!(changes.issues.len(), 2);
    let a = &changes.issues[0];
    assert_eq!(a.id.to_string(), "GetFelix/felix#812");
    assert_eq!(a.labels, ["agent-ok", "p1"]);
    assert_eq!(
        (a.kind.as_deref(), a.author.as_str()),
        (Some("Bug"), "alice")
    );
    let b = &changes.issues[1];
    assert_eq!(b.closed, Some(Closed::Dropped));
    assert_eq!(b.milestone.as_deref(), Some("0.8"));
    assert_eq!(b.assignees, ["carol"]);
    // The cursor sits at the newest update, and the first poll asks for open issues only.
    assert_eq!(
        cur(&changes).issues_since.as_deref(),
        Some("2026-11-10T12:00:00Z")
    );
    assert!(gh.sent()[0].url.contains("state=open"));
}

#[tokio::test]
async fn closed_reasons_map_to_done_or_dropped() {
    let read = |reason: Value| {
        let mut v = issue(1, |_| {});
        v["state"] = json!("closed");
        v["state_reason"] = reason;
        parse_issue("GetFelix/felix", &v).unwrap().closed
    };
    assert_eq!(read(json!("completed")), Some(Closed::Completed));
    assert_eq!(read(json!("not_planned")), Some(Closed::Dropped));
    assert_eq!(read(json!("duplicate")), Some(Closed::Dropped));
    // A reason this version does not know stops dependents instead of freeing them.
    assert_eq!(read(json!("something_new")), Some(Closed::Dropped));
    // No reason is an issue closed before GitHub kept one.
    assert_eq!(read(Value::Null), Some(Closed::Completed));
}

#[tokio::test]
async fn pages_are_followed_and_a_cut_listing_resumes_where_it_stopped() {
    let gh = Script::default();
    let next = "<https://api.github.com/repos/GetFelix/felix/issues?page=2>; rel=\"next\", <https://api.github.com/x?page=9>; rel=\"last\"";
    gh.on(
        "GET",
        "/issues?state",
        with(ok(json!([issue(1, |_| {})])), &[("link", next)]),
    );
    let mut second = issue(2, |_| {});
    second["updated_at"] = json!("2026-11-10T11:00:00Z");
    gh.on("GET", "page=2", ok(json!([second])));
    let changes = github(&gh).changed(&repos(), &none()).await.unwrap();
    assert_eq!(changes.issues.len(), 2);
    assert_eq!(
        cur(&changes).issues_since.as_deref(),
        Some("2026-11-10T11:00:00Z")
    );
    // A multi-page answer is never cached by its first page.
    assert!(cur(&changes).etags.is_empty());
}

#[tokio::test]
async fn a_truncated_listing_of_only_pull_requests_still_moves_the_cursor() {
    let gh = Script::default();
    let next = "<https://api.github.com/repos/GetFelix/felix/issues?page=2>; rel=\"next\"";
    let mut pr = issue(900, |_| {});
    pr["pull_request"] = json!({ "url": "x" });
    pr["updated_at"] = json!("2026-11-10T09:00:00Z");
    gh.on(
        "GET",
        "/issues?state",
        with(ok(json!([pr.clone()])), &[("link", next)]),
    );
    pr["updated_at"] = json!("2026-11-10T10:00:00Z");
    gh.on("GET", "page=2", with(ok(json!([pr])), &[("link", next)]));
    let before = Cursor {
        issues_since: Some("2026-11-01T00:00:00Z".into()),
        ..Cursor::default()
    };
    let changes = github(&gh)
        .changed(&repos(), &at_repo(before.clone()))
        .await
        .unwrap();
    assert!(changes.issues.is_empty());
    assert_eq!(
        cur(&changes).issues_since.as_deref(),
        Some("2026-11-10T10:00:00Z")
    );
}

#[tokio::test]
async fn a_missing_repo_or_a_malformed_item_does_not_stop_the_other_repos() {
    let gh = Script::default();
    gh.on("GET", "/repos/Gone/repo/issues?", status(404, &[]));
    let mut bad = issue(5, |_| {});
    bad.as_object_mut().unwrap().remove("number");
    gh.on(
        "GET",
        "/repos/GetFelix/felix/issues?",
        ok(json!([bad, issue(6, |_| {})])),
    );
    let repos = vec!["Gone/repo".to_owned(), "GetFelix/felix".to_owned()];
    let changes = github(&gh).changed(&repos, &none()).await.unwrap();
    assert_eq!(changes.issues.len(), 1);
    assert_eq!(changes.issues[0].id.number, 6);
    // The missing repo is reported and has no cursor; the other repo's moved.
    assert_eq!(changes.denied.get("Gone/repo"), Some(&404));
    assert!(!changes.cursors.contains_key("Gone/repo"));
    assert!(changes.cursors.contains_key("GetFelix/felix"));
}

#[tokio::test]
async fn an_unchanged_listing_is_asked_with_its_etag_and_costs_nothing() {
    let gh = Script::default();
    gh.on("GET", "/issues/comments?", ok(json!([])));
    gh.on(
        "GET",
        "/issues?",
        with(ok(json!([issue(812, |_| {})])), &[("etag", "\"v1\"")]),
    );
    let g = github(&gh);
    let first = g.changed(&repos(), &none()).await.unwrap();
    // Later polls name `since`, so they have their own validator.
    let second = g.changed(&repos(), &first.cursors).await.unwrap();
    gh.on(
        "GET",
        "/issues?",
        with(ok(json!([issue(812, |_| {})])), &[("etag", "\"v2\"")]),
    );
    let second = g.changed(&repos(), &second.cursors).await.unwrap();
    assert_eq!(cur(&second).etags.values().collect::<Vec<_>>(), ["\"v2\""]);
    gh.on("GET", "/issues?", status(304, &[]));
    let third = g.changed(&repos(), &second.cursors).await.unwrap();
    assert!(third.issues.is_empty());
    let last = gh
        .sent()
        .into_iter()
        .rfind(|r| r.url.contains("/issues?"))
        .unwrap();
    assert_eq!(last.if_none_match.as_deref(), Some("\"v2\""));
    // Nothing changed, so the cursor stays.
    assert_eq!(third.cursors, second.cursors);
}

#[tokio::test]
async fn a_spent_rate_limit_stops_calls_until_it_resets() {
    let gh = Script::default();
    let reset = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 120;
    gh.on(
        "GET",
        "/issues?",
        with(
            status(403, &[]),
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", &reset.to_string()),
            ],
        ),
    );
    let g = github(&gh);
    let err = g.changed(&repos(), &none()).await.unwrap_err();
    let SourceError::RateLimited { wait } = err else {
        panic!("{err}")
    };
    assert!((100..=121).contains(&wait.as_secs()));
    let sent = gh.sent().len();
    // The next call fails without spending a request.
    assert!(matches!(
        g.fetch(&IssueRef::new("GetFelix/felix", 1)).await,
        Err(SourceError::RateLimited { .. })
    ));
    assert_eq!(gh.sent().len(), sent);
}

#[tokio::test]
async fn retry_after_is_honoured() {
    let gh = Script::default();
    gh.on(
        "GET",
        "/issues/1",
        with(status(429, &[]), &[("retry-after", "30")]),
    );
    let err = github(&gh)
        .fetch(&IssueRef::new("GetFelix/felix", 1))
        .await
        .unwrap_err();
    assert!(matches!(err, SourceError::RateLimited { wait } if wait.as_secs() == 30));
}

#[tokio::test]
async fn fetch_follows_a_transfer_and_reads_a_missing_issue_as_gone() {
    let gh = Script::default();
    gh.on(
        "GET",
        "/issues/812",
        status(
            301,
            &[(
                "location",
                "https://api.github.com/repos/GetFelix/felix2/issues/5",
            )],
        ),
    );
    gh.on("GET", "/issues/813", status(404, &[]));
    let mut pr = issue(814, |_| {});
    pr["pull_request"] = json!({});
    gh.on("GET", "/issues/814", ok(pr));
    let g = github(&gh);
    let r = |n| IssueRef::new("GetFelix/felix", n);
    assert_eq!(
        g.fetch(&r(812)).await.unwrap(),
        Fetched::Moved(IssueRef::new("GetFelix/felix2", 5))
    );
    assert_eq!(g.fetch(&r(813)).await.unwrap(), Fetched::Gone);
    assert_eq!(g.fetch(&r(814)).await.unwrap(), Fetched::Gone);
}

#[tokio::test]
async fn a_pull_request_that_is_merged_or_closed_is_left_alone() {
    let gh = Script::default();
    let mut pull = fixture("pull");
    pull["state"] = json!("closed");
    gh.on("GET", "/pulls/2210", ok(pull));
    github(&gh)
        .write_back(&WriteBack::DraftPullRequest {
            mark: "<!-- kbl:t -->".into(),
            pr: IssueRef::new("GetFelix/felix", 2210),
            note: "x".into(),
        })
        .await
        .unwrap();
    assert!(gh.writes().is_empty());
}

#[test]
fn link_headers_and_redirect_paths_parse() {
    assert_eq!(
        next_link("<https://a/b?page=3>; rel=\"next\", <https://a/b?page=9>; rel=\"last\""),
        Some("https://a/b?page=3".into())
    );
    assert_eq!(next_link("<https://a/b?page=1>; rel=\"prev\""), None);
    assert_eq!(
        issue_path("https://api.github.com/repos/o/r/issues/7"),
        Some(IssueRef::new("o/r", 7))
    );
    assert_eq!(issue_path("https://api.github.com/repos/o/r/pulls/7"), None);
}

#[tokio::test]
async fn a_repo_that_answers_404_stalls_only_itself() {
    let gh = Script::default();
    gh.on(
        "GET",
        "/repos/A/a/issues?",
        ok(json!([
            issue(1, |i| i["updated_at"] = json!("2026-11-10T12:00:00Z"))
        ])),
    );
    gh.on("GET", "/repos/B/b/issues?", status(404, &[]));
    gh.on("GET", "/issues/comments?", ok(json!([])));
    let repos = vec!["A/a".to_owned(), "B/b".to_owned()];
    let before = Cursor {
        issues_since: Some("2026-11-01T00:00:00Z".into()),
        ..Cursor::default()
    };
    let both = BTreeMap::from([
        ("A/a".to_owned(), before.clone()),
        ("B/b".to_owned(), before.clone()),
    ]);
    let g = github(&gh);
    let first = g.changed(&repos, &both).await.unwrap();
    assert_eq!(first.issues.len(), 1);
    // A moved on; B answered 404, so it is reported and its cursor is not returned to move.
    assert_eq!(
        first.cursors["A/a"].issues_since.as_deref(),
        Some("2026-11-10T12:00:00Z")
    );
    assert_eq!(first.denied, BTreeMap::from([("B/b".to_owned(), 404)]));
    assert!(!first.cursors.contains_key("B/b"));

    // Once B is readable it resumes from its own old cursor, not from A's.
    gh.on(
        "GET",
        "/repos/B/b/issues?",
        ok(json!([
            issue(2, |i| i["updated_at"] = json!("2026-11-05T00:00:00Z"))
        ])),
    );
    let mut saved = both;
    saved.extend(first.cursors);
    let second = g.changed(&repos, &saved).await.unwrap();
    assert!(second.issues.iter().any(|i| i.id.repo == "B/b"));
    assert!(second.denied.is_empty());
    assert_eq!(
        second.cursors["B/b"].issues_since.as_deref(),
        Some("2026-11-05T00:00:00Z")
    );
    let b_url = gh
        .sent()
        .into_iter()
        .rfind(|r| r.url.contains("/repos/B/b/issues?"))
        .unwrap()
        .url;
    assert!(b_url.contains("since=2026-11-01T00:00:00Z"), "{b_url}");
}

#[tokio::test]
async fn a_pull_request_by_someone_else_is_left_alone() {
    let gh = Script::default();
    let mut pull = fixture("pull");
    pull["user"]["login"] = json!("mallory");
    gh.on("GET", "/pulls/2210", ok(pull));
    let g = Github::new(
        "https://api.github.com",
        gh.clone(),
        vec!["KBL[bot]".into()],
    );
    g.write_back(&WriteBack::DraftPullRequest {
        mark: "<!-- kbl:t -->".into(),
        pr: IssueRef::new("GetFelix/felix", 2210),
        note: "x".into(),
    })
    .await
    .unwrap();
    assert!(gh.writes().is_empty());
}

fn runs(list: &[(&str, &str)]) -> Value {
    let list: Vec<Value> = list
        .iter()
        .map(|(n, c)| json!({ "name": n, "conclusion": c, "html_url": format!("u/{n}") }))
        .collect();
    json!({ "total_count": list.len(), "check_runs": list })
}

fn nostatus() -> Value {
    json!({ "statuses": [] })
}

fn req(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

#[test]
fn a_required_check_with_no_result_yet_is_pending_not_green() {
    let r = req(&["unit", "lint"]);
    let (checks, failing) = check_state(
        Some(&runs(&[("unit", "success")])),
        Some(&nostatus()),
        &[],
        Some(&r),
    );
    assert_eq!((checks, failing), (PrChecks::Pending, None));
    let (checks, _) = check_state(
        Some(&runs(&[("unit", "success"), ("lint", "success")])),
        Some(&nostatus()),
        &[],
        Some(&r),
    );
    assert_eq!(checks, PrChecks::Green);
}

#[test]
fn an_empty_required_list_means_unknown_and_every_check_counts() {
    let none: Vec<String> = vec![];
    let (checks, failing) = check_state(
        Some(&runs(&[("nightly", "failure")])),
        Some(&nostatus()),
        &[],
        Some(&none),
    );
    assert_eq!(checks, PrChecks::Red);
    assert_eq!(failing.unwrap().name, "nightly");
    let (checks, _) = check_state(
        Some(&runs(&[("unit", "success")])),
        Some(&nostatus()),
        &[],
        Some(&none),
    );
    assert_eq!(checks, PrChecks::Green);
}

#[test]
fn a_required_context_can_be_a_commit_status() {
    let r = req(&["unit", "ci/legacy"]);
    let statuses = |state: &str| json!({ "statuses": [{ "context": "ci/legacy", "state": state, "target_url": "u/legacy" }] });
    let ok = runs(&[("unit", "success")]);
    let (checks, _) = check_state(Some(&ok), Some(&statuses("success")), &[], Some(&r));
    assert_eq!(checks, PrChecks::Green);
    let (checks, failing) = check_state(Some(&ok), Some(&statuses("failure")), &[], Some(&r));
    assert_eq!(checks, PrChecks::Red);
    assert_eq!(failing.unwrap().url, "u/legacy");
    let (checks, _) = check_state(Some(&ok), Some(&statuses("pending")), &[], Some(&r));
    assert_eq!(checks, PrChecks::Pending);
    // Without a status for it, the required context is still outstanding.
    let (checks, _) = check_state(Some(&ok), Some(&nostatus()), &[], Some(&r));
    assert_eq!(checks, PrChecks::Pending);
}

#[test]
fn github_times_parse_to_epoch_milliseconds() {
    assert_eq!(parse_utc_ms("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(
        parse_utc_ms("2027-01-15T08:00:00Z"),
        Some(1_800_000_000_000)
    );
    assert_eq!(
        parse_utc_ms("2024-02-29T12:30:01Z"),
        Some(1_709_209_801_000)
    );
    assert_eq!(parse_utc_ms("2027-13-15T08:00:00Z"), None);
    assert_eq!(parse_utc_ms("yesterday"), None);
}

#[test]
fn a_status_written_by_our_own_credential_is_ignored() {
    let ok = runs(&[("unit", "success")]);
    let forged = json!({ "statuses": [
        { "context": "ci/legacy", "state": "success", "target_url": "u", "creator": { "login": "KBL[bot]" } },
    ] });
    let own = ["kbl[bot]".to_owned()];
    // Required, and only satisfied by our own forged status: still outstanding.
    let r = req(&["unit", "ci/legacy"]);
    let (checks, _) = check_state(Some(&ok), Some(&forged), &own, Some(&r));
    assert_eq!(checks, PrChecks::Pending);
    // A forged green cannot hide nothing, and a real one by another login counts.
    let real = json!({ "statuses": [
        { "context": "ci/legacy", "state": "success", "target_url": "u", "creator": { "login": "ci-bot" } },
    ] });
    let (checks, _) = check_state(Some(&ok), Some(&real), &own, Some(&r));
    assert_eq!(checks, PrChecks::Green);
}

#[test]
fn a_failed_listing_is_unknown_and_never_green_or_no_checks() {
    let red = json!({ "statuses": [
        { "context": "ci/x", "state": "failure", "target_url": "u/x", "creator": { "login": "ci" } },
    ] });
    // Check runs unavailable: no statuses means pending, not "no checks".
    let (checks, _) = check_state(None, Some(&nostatus()), &[], None);
    assert_eq!(checks, PrChecks::Pending);
    // A red status is kept.
    let (checks, failing) = check_state(None, Some(&red), &[], None);
    assert_eq!(
        (checks, failing.map(|f| f.name)),
        (PrChecks::Red, Some("ci/x".into()))
    );
    // Statuses unavailable beside green runs is unknown too.
    let (checks, _) = check_state(Some(&runs(&[("unit", "success")])), None, &[], None);
    assert_eq!(checks, PrChecks::Pending);
    // Fewer statuses than GitHub counts.
    let short = json!({ "statuses": [], "incomplete": true });
    let (checks, _) = check_state(Some(&runs(&[("unit", "success")])), Some(&short), &[], None);
    assert_eq!(checks, PrChecks::Pending);
}

#[tokio::test]
async fn commit_statuses_are_paged_so_none_are_lost_past_a_hundred() {
    let gh = Script::default();
    let page = |from: usize, n: usize| -> Value {
        let list: Vec<Value> = (from..from + n)
            .map(|i| json!({ "context": format!("c{i}"), "state": "success", "creator": { "login": "ci" } }))
            .collect();
        json!({ "total_count": 130, "statuses": list })
    };
    gh.on("GET", "page=1", ok(page(0, 100)));
    gh.on("GET", "page=2", ok(page(100, 30)));
    let got = github(&gh)
        .commit_statuses("GetFelix/felix", "abc")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got["statuses"].as_array().unwrap().len(), 130);
    assert_eq!(got["incomplete"], false);
    // A page that comes back short leaves it marked incomplete.
    gh.on(
        "GET",
        "page=2",
        ok(json!({ "total_count": 130, "statuses": [] })),
    );
    let got = github(&gh)
        .commit_statuses("GetFelix/felix", "abc")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got["incomplete"], true);
    gh.on("GET", "page=1", status(500, &[]));
    assert!(
        github(&gh)
            .commit_statuses("GetFelix/felix", "abc")
            .await
            .is_err()
    );
}

#[test]
fn a_check_run_by_our_own_app_does_not_count() {
    let r = req(&["unit"]);
    let run = |app: &str| {
        json!({ "total_count": 1, "check_runs": [{
            "name": "unit", "conclusion": "success", "html_url": "u/unit", "app": { "slug": app }
        }] })
    };
    let own = req(&["KBL[bot]"]);
    // An agent with the App's token could post the required check as passed.
    let (checks, _) = check_state(Some(&run("kbl")), Some(&nostatus()), &own, Some(&r));
    assert_eq!(checks, PrChecks::Pending);
    let (checks, _) = check_state(
        Some(&run("github-actions")),
        Some(&nostatus()),
        &own,
        Some(&r),
    );
    assert_eq!(checks, PrChecks::Green);
}

#[tokio::test]
async fn releases_skip_drafts_prereleases_and_odd_tags_and_come_oldest_first() {
    let gh = Script::default();
    let rel = |tag: &str, at: &str, draft: bool, pre: bool| json!({ "tag_name": tag, "published_at": at, "draft": draft, "prerelease": pre });
    gh.on(
        "GET",
        "/repos/GetFelix/felix/releases?",
        ok(json!([
            rel("v2", "2027-01-15T12:00:00Z", false, false),
            rel("v3-draft", "2027-01-15T13:00:00Z", true, false),
            rel("v3-rc", "2027-01-15T13:00:00Z", false, true),
            rel("a b?x", "2027-01-15T11:00:00Z", false, false),
            rel("v1", "2027-01-15T10:00:00Z", false, false),
        ])),
    );
    let got = github(&gh).releases("GetFelix/felix", 0).await.unwrap();
    let tags: Vec<_> = got.list.iter().map(|r| r.tag.as_str()).collect();
    assert_eq!(tags, ["v1", "v2"]);
    assert!(got.whole && got.covers(0));
}

#[tokio::test]
async fn a_tag_contains_a_commit_only_when_the_compare_is_ahead_or_identical() {
    let gh = Script::default();
    gh.on("GET", "/compare/aaa...v1", ok(json!({ "status": "ahead" })));
    gh.on(
        "GET",
        "/compare/aaa...v2",
        ok(json!({ "status": "identical" })),
    );
    gh.on(
        "GET",
        "/compare/aaa...v3",
        ok(json!({ "status": "behind" })),
    );
    let g = github(&gh);
    assert!(g.contains("GetFelix/felix", "v1", "aaa").await.unwrap());
    assert!(g.contains("GetFelix/felix", "v2", "aaa").await.unwrap());
    assert!(!g.contains("GetFelix/felix", "v3", "aaa").await.unwrap());
    assert!(!g.contains("GetFelix/felix", "v4", "aaa").await.unwrap());
    assert!(!g.contains("GetFelix/felix", "../x?y", "aaa").await.unwrap());
}

#[tokio::test]
async fn releases_page_back_to_the_merge_and_no_further_than_five_pages() {
    let rel = |n: u64| {
        let at = format!(
            "2027-01-{:02}T{:02}:{:02}:00Z",
            10 + n / 600,
            n / 60 % 10 + 10,
            n % 60
        );
        json!({ "tag_name": format!("v{n}"), "published_at": at, "draft": false, "prerelease": false })
    };
    let page = |from: u64| Value::Array((0..100).map(|i| rel(9_000 - from - i)).collect());
    let gh = Script::default();
    for n in 1..=6u64 {
        gh.on(
            "GET",
            &format!("/releases?per_page=100&page={n}"),
            ok(page((n - 1) * 100)),
        );
    }
    let g = github(&gh);
    let pages = |gh: &Script| {
        gh.sent()
            .iter()
            .filter(|r| r.url.contains("/releases?"))
            .count()
    };
    // Everything is newer than the merge: five pages, then it stops, and a
    // merge older than every page read is not covered.
    let all = g.releases("GetFelix/felix", 0).await.unwrap();
    assert_eq!(pages(&gh), 5);
    assert_eq!(all.list.len(), 500);
    assert!(
        all.list
            .windows(2)
            .all(|w| w[0].published_ms <= w[1].published_ms)
    );
    let oldest = all.list[0].published_ms;
    assert!(!all.whole);
    assert!(!all.covers(oldest) && !all.covers(0));
    assert!(all.covers(oldest + 1));
    // A release older than the merge on page 2: one more page, then it stops.
    let since = all.list[350].published_ms;
    let some = g.releases("GetFelix/felix", since).await.unwrap();
    assert_eq!(pages(&gh), 8);
    assert_eq!(some.list.len(), 150);
    assert!(some.list.iter().all(|r| r.published_ms >= since));
    assert!(some.covers(since));
    // First seen on the last page: the cap still holds.
    let since = all.list[50].published_ms;
    let last = g.releases("GetFelix/felix", since).await.unwrap();
    assert_eq!(pages(&gh), 13);
    assert_eq!(last.list.len(), 450);
    assert!(last.covers(since));
    // A short page is the end of the list.
    gh.on("GET", "/releases?per_page=100&page=2", ok(json!([rel(1)])));
    let short = g.releases("GetFelix/felix", 0).await.unwrap();
    assert_eq!(pages(&gh), 15);
    assert!(short.whole && short.covers(0));
}

#[tokio::test]
async fn a_draft_note_whose_post_may_have_landed_is_not_posted_again() {
    let gh = Script::default();
    let mut pull = fixture("pull");
    pull["draft"] = json!(true);
    gh.on("GET", "/pulls/2210", ok(pull));
    let g = Github::new(
        "https://api.github.com",
        gh.clone(),
        vec!["kbl[bot]".into()],
    );
    let draft = WriteBack::DraftPullRequest {
        mark: "<!-- kbl:GetFelix/felix#9:draft:GetFelix/felix#2210 -->".into(),
        pr: IssueRef::new("GetFelix/felix", 2210),
        note: "x".into(),
    };
    gh.on("GET", "/issues/2210/comments", ok(json!([])));
    gh.on("POST", "/issues/2210/comments", status(201, &[]));
    g.write_back(&draft).await.unwrap();
    let posted = gh.sent().into_iter().rfind(|r| r.method == "POST").unwrap();
    assert_eq!(
        posted.body.unwrap()["body"],
        "x\n\n<!-- kbl:GetFelix/felix#9:draft:GetFelix/felix#2210 -->"
    );
    // The first post was cut short but landed: the retry finds its mark.
    let landed = json!([{
        "id": 1,
        "issue_url": "https://api.github.com/repos/GetFelix/felix/issues/2210",
        "user": { "login": "kbl[bot]" },
        "body": "x\n\n<!-- kbl:GetFelix/felix#9:draft:GetFelix/felix#2210 -->",
    }]);
    gh.on("GET", "/issues/2210/comments", ok(landed));
    g.write_back(&draft).await.unwrap();
    assert_eq!(gh.writes().len(), 1, "{:?}", gh.writes());
}

#[test]
fn only_an_actions_job_link_of_the_repo_names_a_run_to_re_run() {
    let url = |p: &str| format!("https://github.com/GetFelix/felix/{p}");
    assert_eq!(
        actions_run("GetFelix/felix", &url("actions/runs/700/job/71")),
        Some(700)
    );
    assert_eq!(
        actions_run("getfelix/FELIX", &url("actions/runs/700/job/71?pr=2")),
        Some(700)
    );
    // A run page without a job, a status link, another repo's run, junk ids.
    assert_eq!(
        actions_run("GetFelix/felix", &url("actions/runs/700")),
        None
    );
    assert_eq!(
        actions_run("GetFelix/felix", "https://ci.example/build/7"),
        None
    );
    assert_eq!(
        actions_run(
            "GetFelix/felix",
            "https://github.com/Other/repo/actions/runs/700/job/71"
        ),
        None
    );
    assert_eq!(
        actions_run("GetFelix/felix", &url("actions/runs/7x/job/71")),
        None
    );
    assert_eq!(
        actions_run("GetFelix/felix", &url("actions/runs/700/job/")),
        None
    );
}

#[test]
fn the_failing_output_is_the_failed_checks_own_text() {
    let runs = json!({ "check_runs": [
        { "name": "unit", "conclusion": "success", "html_url": "u/unit",
          "output": { "title": "ok", "summary": "fine", "text": null } },
        { "name": "lint", "conclusion": "failure", "html_url": "u/lint",
          "output": { "title": "Lint failed", "summary": "timed out", "text": "line 4" } },
    ] });
    let f = FailedCheck {
        name: "lint".into(),
        url: "u/lint".into(),
    };
    let out = failing_output(Some(&runs), None, &f);
    assert!(out.contains("Lint failed") && out.contains("timed out") && out.contains("line 4"));
    assert!(!out.contains("fine"));

    let statuses = json!({ "statuses": [
        { "context": "lint", "target_url": "u/lint", "description": "queue timed out" },
    ] });
    assert_eq!(failing_output(None, Some(&statuses), &f), "queue timed out");
    assert_eq!(failing_output(None, None, &f), "");
}
