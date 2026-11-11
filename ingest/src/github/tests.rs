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
    let changes = github(&gh)
        .changed(&repos(), &Cursor::default())
        .await
        .unwrap();
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
        changes.cursor.issues_since.as_deref(),
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
    assert_eq!(read(Value::Null), Some(Closed::Dropped));
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
    let changes = github(&gh)
        .changed(&repos(), &Cursor::default())
        .await
        .unwrap();
    assert_eq!(changes.issues.len(), 2);
    assert_eq!(
        changes.cursor.issues_since.as_deref(),
        Some("2026-11-10T11:00:00Z")
    );
    // A multi-page answer is never cached by its first page.
    assert!(changes.cursor.etags.is_empty());
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
    let changes = github(&gh).changed(&repos(), &before).await.unwrap();
    assert!(changes.issues.is_empty());
    assert_eq!(
        changes.cursor.issues_since.as_deref(),
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
    let changes = github(&gh)
        .changed(&repos, &Cursor::default())
        .await
        .unwrap();
    assert_eq!(changes.issues.len(), 1);
    assert_eq!(changes.issues[0].id.number, 6);
    assert!(changes.cursor.issues_since.is_some());
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
    let first = g.changed(&repos(), &Cursor::default()).await.unwrap();
    // Later polls name `since`, so they have their own validator.
    let second = g.changed(&repos(), &first.cursor).await.unwrap();
    gh.on(
        "GET",
        "/issues?",
        with(ok(json!([issue(812, |_| {})])), &[("etag", "\"v2\"")]),
    );
    let second = g.changed(&repos(), &second.cursor).await.unwrap();
    assert_eq!(second.cursor.etags.values().collect::<Vec<_>>(), ["\"v2\""]);
    gh.on("GET", "/issues?", status(304, &[]));
    let third = g.changed(&repos(), &second.cursor).await.unwrap();
    assert!(third.issues.is_empty());
    let last = gh
        .sent()
        .into_iter()
        .rfind(|r| r.url.contains("/issues?"))
        .unwrap();
    assert_eq!(last.if_none_match.as_deref(), Some("\"v2\""));
    // Nothing changed, so the cursor stays.
    assert_eq!(third.cursor, second.cursor);
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
    let err = g.changed(&repos(), &Cursor::default()).await.unwrap_err();
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
