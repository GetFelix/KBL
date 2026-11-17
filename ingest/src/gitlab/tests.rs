use std::sync::{Arc, Mutex};

use kbl_core::work::{PrChecks, PrPhase, PrReview, WorkRecord, pr_branch};
use serde_json::{Value, json};

use super::*;
use crate::ado::transport::Secret;
use crate::config::Config;
use crate::sync::{Ingest, Settings};
use crate::testing::Fake;

type Handler = dyn Fn(&Req) -> Res + Send + Sync;

const PROJECT: &str = "/api/v4/projects/group%2Fsub%2Fapp";

/// GitLab as hand-written: a handler answers each request and every request
/// is kept.
#[derive(Clone)]
struct Lab {
    handler: Arc<Handler>,
    sent: Arc<Mutex<Vec<Req>>>,
}

impl Lab {
    fn new(handler: impl Fn(&Req) -> Res + Send + Sync + 'static) -> Self {
        Self {
            handler: Arc::new(handler),
            sent: Arc::default(),
        }
    }

    fn writes(&self) -> Vec<(String, Value)> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method != "GET")
            .map(|r| {
                (
                    format!("{} {}", r.method, path(r)),
                    r.body.clone().unwrap_or(Value::Null),
                )
            })
            .collect()
    }

    fn urls(&self) -> Vec<String> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.url.clone())
            .collect()
    }
}

impl Transport for Lab {
    async fn send(&self, req: Req) -> anyhow::Result<Res> {
        let res = (self.handler)(&req);
        self.sent.lock().unwrap().push(req);
        Ok(res)
    }
}

fn res(status: u16, body: Value) -> Res {
    Res {
        status,
        headers: BTreeMap::new(),
        body,
    }
}

fn path(req: &Req) -> String {
    reqwest::Url::parse(&req.url).unwrap().path().to_owned()
}

fn param(req: &Req, key: &str) -> Option<String> {
    reqwest::Url::parse(&req.url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn raw(iid: u64, edit: impl FnOnce(&mut Value)) -> Value {
    let mut v = json!({
        "id": 9000 + iid,
        "iid": iid,
        "title": "Stale read after failover",
        "description": "Reads go stale.\nTwice.",
        "state": "opened",
        "labels": ["agent-ok", "priority::1"],
        "milestone": { "title": "0.8" },
        "assignees": [{ "username": "carol" }],
        "author": { "username": "alice" },
        "type": "ISSUE",
        "web_url": "https://gitlab.example.com/group/sub/app/-/issues/12",
        "user_notes_count": 0,
        "moved_to_id": null,
        "updated_at": "2026-11-10T12:00:05.123Z"
    });
    edit(&mut v);
    v
}

fn config_json(extra: Value) -> Value {
    let mut rule = json!({
        "name": "agent-ok", "repo": "team/app", "labels_all": ["agent-ok"],
        "mode": "admit",
        "priority_labels": { "priority::1": 0 },
        "comment": true, "transition_on_merge": "close"
    });
    rule.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    json!({
        "tracker": { "url": "http://t", "token_env": "T" },
        "gitlab": {
            "api": "https://gitlab.example.com",
            "projects": { "team/app": "group/sub/app" },
            "token_env": "GITLAB_TOKEN"
        },
        "rules": [rule],
    })
}

fn config(extra: Value) -> Config {
    Config::parse(&config_json(extra).to_string()).unwrap()
}

fn gitlab_with(lab: &Lab, cfg: &Config) -> Gitlab<Lab> {
    let g = cfg.gitlab.as_ref().unwrap();
    Gitlab::new(
        &g.api,
        lab.clone(),
        vec!["kbl-bot".into()],
        cfg.rules.clone(),
        &g.projects,
    )
    .unwrap()
}

fn gitlab(lab: &Lab) -> Gitlab<Lab> {
    gitlab_with(lab, &config(json!({})))
}

fn none() -> BTreeMap<String, Cursor> {
    BTreeMap::new()
}

fn repos() -> Vec<String> {
    vec!["team/app".to_owned()]
}

/// A project holding these issues.
fn listing(issues: Vec<Value>) -> Lab {
    Lab::new(move |req| {
        let p = path(req);
        if p == format!("{PROJECT}/issues") {
            return res(200, Value::Array(issues.clone()));
        }
        if let Some(n) = p
            .strip_prefix(&format!("{PROJECT}/issues/"))
            .and_then(|n| n.parse::<u64>().ok())
        {
            return match issues.iter().find(|i| i["iid"] == n) {
                Some(i) => res(200, i.clone()),
                None => res(404, Value::Null),
            };
        }
        if p.ends_with("/notes") && req.method == "GET" {
            return res(200, json!([]));
        }
        res(200, Value::Null)
    })
}

#[tokio::test]
async fn fields_map_and_labels_select_the_issue() {
    let lab = listing(vec![
        raw(12, |_| {}),
        raw(13, |v| {
            v["state"] = json!("closed");
            v["labels"] = json!(["agent-ok", "wontfix"]);
        }),
        raw(14, |v| v["state"] = json!("closed")),
        raw(15, |v| {
            v["state"] = json!("closed");
            v["moved_to_id"] = json!(77);
        }),
    ]);
    let changes = gitlab(&lab).changed(&repos(), &none()).await.unwrap();
    let a = &changes.issues[0];
    assert_eq!(a.id.to_string(), "team/app#12");
    assert_eq!(a.title, "Stale read after failover");
    assert_eq!(a.body, "Reads go stale.\nTwice.");
    assert_eq!(a.labels, ["agent-ok", "priority::1"]);
    assert_eq!(a.milestone.as_deref(), Some("0.8"));
    assert_eq!(a.assignees, ["carol"]);
    assert_eq!(a.author, "alice");
    assert_eq!(a.kind.as_deref(), Some("ISSUE"));
    assert_eq!(a.closed, None);
    assert_eq!(changes.issues[1].closed, Some(Closed::Dropped));
    assert_eq!(changes.issues[2].closed, Some(Closed::Completed));
    assert_eq!(changes.issues[3].closed, Some(Closed::Dropped));
    // The project path is one encoded segment, and the first poll reads open issues.
    let first = &lab.sent.lock().unwrap()[0];
    assert_eq!(path(first), format!("{PROJECT}/issues"));
    assert_eq!(param(first, "state").as_deref(), Some("opened"));
    assert_eq!(param(first, "updated_after"), None);
    assert_eq!(
        changes.cursors["team/app"].issues_since.as_deref(),
        Some("2026-11-10T12:00:05.123Z")
    );
    let cfg = config(json!({}));
    assert!(cfg.rules[0].matches(a));
    // The priority label maps to a tier.
    assert_eq!(cfg.rules[0].tier_for(a), 0);
}

#[tokio::test]
async fn milestone_assignee_and_type_select_like_github() {
    let lab = listing(vec![raw(12, |_| {})]);
    let changes = gitlab(&lab).changed(&repos(), &none()).await.unwrap();
    let a = &changes.issues[0];
    let rule = |extra: Value| config(extra).rules[0].clone();
    assert!(rule(json!({ "milestone": "0.8" })).matches(a));
    assert!(!rule(json!({ "milestone": "0.9" })).matches(a));
    assert!(rule(json!({ "assignee": "Carol" })).matches(a));
    assert!(!rule(json!({ "assignee": "dave" })).matches(a));
    assert!(rule(json!({ "type": "issue" })).matches(a));
    assert!(!rule(json!({ "type": "incident" })).matches(a));
    assert!(!rule(json!({ "labels_excluded": ["priority::1"] })).matches(a));
    // A project not in the rule's repo is not selected.
    let mut other = a.clone();
    other.id = IssueRef::new("team/other", 12);
    assert!(!rule(json!({})).matches(&other));
}

#[tokio::test]
async fn notes_are_read_only_for_a_wanted_issue_and_system_notes_are_left_out() {
    let lab = Lab::new(|req| {
        let p = path(req);
        if p == format!("{PROJECT}/issues") {
            return res(
                200,
                json!([
                    raw(12, |v| v["user_notes_count"] = json!(3)),
                    raw(13, |v| v["labels"] = json!(["other"])),
                    raw(14, |_| {}),
                ]),
            );
        }
        if p == format!("{PROJECT}/issues/12/notes") {
            return res(
                200,
                json!([
                    { "id": 1, "body": "Seen on 0.7 too.", "author": { "username": "bob" },
                      "system": false, "internal": false, "updated_at": "2026-11-10T12:01:00.000Z" },
                    { "id": 2, "body": "changed milestone", "author": { "username": "bob" },
                      "system": true, "updated_at": "2026-11-10T12:02:00.000Z" },
                    { "id": 3, "body": "Staff only.", "author": { "username": "bob" },
                      "system": false, "internal": true, "updated_at": "2026-11-10T12:03:00.000Z" },
                ]),
            );
        }
        res(404, Value::Null)
    });
    let changes = gitlab(&lab).changed(&repos(), &none()).await.unwrap();
    let bodies: Vec<&str> = changes.comments.iter().map(|c| c.body.as_str()).collect();
    assert_eq!(bodies, ["Seen on 0.7 too."]);
    assert_eq!(changes.comments[0].issue.to_string(), "team/app#12");
    let urls = lab.urls();
    // 13 is no rule's, and 14 has no notes.
    assert!(
        urls.iter().all(|u| !u.contains("/issues/13/notes")),
        "{urls:?}"
    );
    assert!(
        urls.iter().all(|u| !u.contains("/issues/14/notes")),
        "{urls:?}"
    );
}

#[tokio::test]
async fn a_later_poll_asks_since_the_cursor_and_a_bad_cursor_is_not_sent() {
    let lab = listing(vec![raw(12, |_| {})]);
    let at = |s: &str| {
        BTreeMap::from([(
            "team/app".to_owned(),
            Cursor {
                issues_since: Some(s.into()),
                ..Cursor::default()
            },
        )])
    };
    gitlab(&lab)
        .changed(&repos(), &at("2026-11-01T09:15:42.000Z"))
        .await
        .unwrap();
    let first = lab.sent.lock().unwrap()[0].clone();
    assert_eq!(
        param(&first, "updated_after").as_deref(),
        Some("2026-11-01T09:15:42.000Z")
    );
    assert_eq!(param(&first, "state").as_deref(), Some("all"));

    // A cursor is saved state: one that is not a time never reaches the address.
    let lab = listing(vec![]);
    gitlab(&lab)
        .changed(&repos(), &at("2026-11-01T09:15:42Z&state=x#../../"))
        .await
        .unwrap();
    let first = lab.sent.lock().unwrap()[0].clone();
    assert_eq!(param(&first, "updated_after"), None);
    assert!(!first.url.contains("state=x"), "{}", first.url);
}

#[tokio::test]
async fn pages_are_asked_by_number_and_a_cut_short_listing_resumes() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let lab = Lab::new(move |req| {
        let page: usize = param(req, "page").unwrap().parse().unwrap();
        seen.lock().unwrap().push(page);
        let mut r = res(
            200,
            json!([raw(10 + page as u64, |v| {
                v["updated_at"] = json!(format!("2026-11-10T12:00:0{page}.000Z"))
            })]),
        );
        // The server's own next-page link would point anywhere; only the number counts.
        r.headers.insert(
            "x-next-page".into(),
            if page < 3 {
                (page + 1).to_string()
            } else {
                String::new()
            },
        );
        r.headers.insert(
            "link".into(),
            "<http://evil.example/x>; rel=\"next\"".into(),
        );
        r
    });
    let changes = gitlab(&lab).changed(&repos(), &none()).await.unwrap();
    assert_eq!(changes.issues.len(), 3);
    assert_eq!(*calls.lock().unwrap(), [1, 2, 3]);
    assert!(
        lab.urls()
            .iter()
            .all(|u| u.starts_with("https://gitlab.example.com/"))
    );
    assert_eq!(
        changes.cursors["team/app"].issues_since.as_deref(),
        Some("2026-11-10T12:00:03.000Z")
    );
}

#[tokio::test]
async fn item_text_never_reaches_an_address() {
    let lab = listing(vec![raw(12, |v| {
        v["title"] = json!("../../../admin x?a=b#c");
        v["labels"] = json!(["agent-ok", "../x", "a/b"]);
        v["milestone"] = json!({ "title": "../../etc" });
        v["web_url"] = json!("http://evil.example/steal");
    })]);
    let g = gitlab(&lab);
    let changes = g.changed(&repos(), &none()).await.unwrap();
    g.links(&changes.issues[0].id).await.unwrap();
    g.issue_comments(&changes.issues[0].id).await.unwrap();
    for r in lab.sent.lock().unwrap().iter() {
        assert!(
            r.url
                .starts_with("https://gitlab.example.com/api/v4/projects/group%2Fsub%2Fapp/"),
            "{}",
            r.url
        );
        assert!(
            !r.url.contains("evil") && !r.url.contains("admin"),
            "{}",
            r.url
        );
    }
}

#[tokio::test]
async fn blocked_by_links_become_edges_and_other_links_do_not() {
    let link = |kind: &str, full: &str| json!({ "iid": 1, "link_type": kind, "references": { "full": full } });
    let lab = Lab::new(move |req| {
        assert!(path(req).ends_with("/issues/12/links"), "{}", req.url);
        res(
            200,
            json!([
                link("is_blocked_by", "group/sub/app#3"),
                link("blocks", "group/sub/app#9"),
                link("relates_to", "group/sub/app#4"),
                link("is_blocked_by", "GROUP/Sub/App#5"),
            ]),
        )
    });
    let links = gitlab(&lab)
        .links(&IssueRef::new("team/app", 12))
        .await
        .unwrap();
    assert_eq!(
        links.blocked_by,
        [IssueRef::new("team/app", 3), IssueRef::new("team/app", 5)]
    );
    assert!(links.sub_issues.is_empty());
    assert!(links.complete);
}

#[tokio::test]
async fn a_blocker_in_an_unserved_project_leaves_the_links_incomplete() {
    let lab = Lab::new(|_| {
        res(
            200,
            json!([{ "link_type": "is_blocked_by", "references": { "full": "elsewhere/proj#3" } }]),
        )
    });
    let links = gitlab(&lab)
        .links(&IssueRef::new("team/app", 12))
        .await
        .unwrap();
    assert!(links.blocked_by.is_empty());
    assert!(!links.complete);
}

#[tokio::test]
async fn an_unreadable_link_listing_is_not_complete_and_a_server_error_fails() {
    let lab = Lab::new(|_| res(404, Value::Null));
    let links = gitlab(&lab)
        .links(&IssueRef::new("team/app", 12))
        .await
        .unwrap();
    assert!(!links.complete);
    let lab = Lab::new(|_| res(500, Value::Null));
    assert!(
        gitlab(&lab)
            .links(&IssueRef::new("team/app", 12))
            .await
            .is_err()
    );
}

// ---- merge requests ----

const OURS: &str = "kbl-bot";

fn mr(edit: impl FnOnce(&mut Value)) -> Value {
    let mut v = json!({
        "iid": 7,
        "title": "Fix the stale read",
        "state": "opened",
        "draft": false,
        "author": { "username": OURS },
        "source_branch": "kbl/w1",
        "target_branch": "main",
        "description": "Closes #12",
        "created_at": "2027-01-15T09:00:00.000Z",
        "merged_at": null,
        "merge_user": null,
        "merge_commit_sha": null,
        "sha": "headsha",
        "reviewers": [],
        "head_pipeline": null
    });
    edit(&mut v);
    v
}

fn merged(edit: impl FnOnce(&mut Value)) -> Value {
    mr(|v| {
        v["state"] = json!("merged");
        v["merged_at"] = json!("2027-01-15T10:00:00.250Z");
        v["merge_user"] = json!({ "username": "carol" });
        v["merge_commit_sha"] = json!("mergesha");
        edit(v);
    })
}

/// A project whose merge request 7 is this one.
fn with_mr(mr: Value, default_branch: Option<&str>) -> Lab {
    let default_branch = default_branch.map(str::to_owned);
    Lab::new(move |req| {
        let p = path(req);
        if p == format!("{PROJECT}/merge_requests/7") {
            return res(200, mr.clone());
        }
        if p == PROJECT {
            return match &default_branch {
                Some(d) => res(200, json!({ "default_branch": d })),
                None => res(404, Value::Null),
            };
        }
        if p.ends_with("/approvals") {
            return res(200, json!({ "approved_by": [] }));
        }
        if p.ends_with("/reviewers") {
            return res(200, json!([]));
        }
        res(404, Value::Null)
    })
}

async fn read(lab: &Lab) -> PullRequest {
    gitlab(lab)
        .pull_request(&IssueRef::new("team/app", 7))
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn a_merge_into_the_default_branch_by_someone_else_counts() {
    let pull = read(&with_mr(merged(|_| {}), Some("main"))).await;
    let status = pull.status.unwrap();
    assert_eq!(status.phase, PrPhase::Merged);
    assert_eq!(status.pr, "team/app#7");
    assert_eq!(pull.head_ref, "kbl/w1");
    assert_eq!(pull.body, "Closes #12");
    assert_eq!(pull.created_ms, Some(1_800_000_000_000 + 3_600_000));
    // Fractions of a second are kept.
    assert_eq!(
        pull.merge,
        Some(("mergesha".into(), 1_800_000_000_000 + 7_200_000 + 250))
    );
}

#[tokio::test]
async fn a_merge_that_does_not_count_is_closed_not_merged() {
    // Into a branch that is not the default.
    let pull = read(&with_mr(
        merged(|v| v["target_branch"] = json!("dev")),
        Some("main"),
    ))
    .await;
    assert_eq!(pull.status.unwrap().phase, PrPhase::Closed);
    assert_eq!(pull.merge, None);
    // By our own credential: an agent never merges its own work.
    let by_us = merged(|v| v["merge_user"] = json!({ "username": "KBL-Bot" }));
    assert_eq!(
        read(&with_mr(by_us, Some("main")))
            .await
            .status
            .unwrap()
            .phase,
        PrPhase::Closed
    );
    // The older field names the merger too.
    let old = merged(|v| {
        v["merge_user"] = Value::Null;
        v["merged_by"] = json!({ "username": OURS });
    });
    assert_eq!(
        read(&with_mr(old, Some("main")))
            .await
            .status
            .unwrap()
            .phase,
        PrPhase::Closed
    );
    // A merger GitLab does not name cannot be shown not to be us.
    let nobody = merged(|v| v["merge_user"] = Value::Null);
    assert_eq!(
        read(&with_mr(nobody, Some("main")))
            .await
            .status
            .unwrap()
            .phase,
        PrPhase::Closed
    );
    // A project whose default branch cannot be read is an error, never a closed request.
    assert!(
        gitlab(&with_mr(merged(|_| {}), None))
            .pull_request(&IssueRef::new("team/app", 7))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_merge_request_of_another_author_is_not_read_further() {
    let lab = with_mr(
        merged(|v| v["author"] = json!({ "username": "mallory" })),
        Some("main"),
    );
    let pull = read(&lab).await;
    assert_eq!(pull.author, "mallory");
    assert!(pull.status.is_none() && pull.merge.is_none());
    // Nothing but the merge request itself was read.
    assert_eq!(lab.urls().len(), 1, "{:?}", lab.urls());
}

#[tokio::test]
async fn a_closed_request_is_closed_and_a_missing_one_is_none() {
    let pull = read(&with_mr(mr(|v| v["state"] = json!("closed")), Some("main"))).await;
    assert_eq!(pull.status.unwrap().phase, PrPhase::Closed);
    let lab = Lab::new(|_| res(404, Value::Null));
    assert!(
        gitlab(&lab)
            .pull_request(&IssueRef::new("team/app", 7))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_request_outside_the_configured_projects_is_never_read() {
    let lab = with_mr(merged(|_| {}), Some("main"));
    let err = gitlab(&lab)
        .pull_request(&IssueRef::new("evil/repo", 7))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not a project"), "{err}");
    assert!(lab.urls().is_empty());
}

#[tokio::test]
async fn an_open_request_reads_reviews_and_the_pipeline() {
    let lab = Lab::new(|req| {
        let p = path(req);
        if p == format!("{PROJECT}/merge_requests/7") {
            return res(
                200,
                mr(|v| {
                    v["reviewers"] = json!([{ "username": "carol" }]);
                    v["head_pipeline"] =
                        json!({ "id": 55, "status": "failed", "web_url": "https://gl/p/55" });
                }),
            );
        }
        if p.ends_with("/approvals") {
            return res(
                200,
                json!({ "approved_by": [{ "user": { "username": OURS } }] }),
            );
        }
        if p.ends_with("/reviewers") {
            return res(
                200,
                json!([{ "user": { "username": "carol" }, "state": "requested_changes" }]),
            );
        }
        if p.ends_with("/pipelines/55/jobs") {
            assert_eq!(param(req, "scope[]").as_deref(), Some("failed"));
            return res(
                200,
                json!([
                    { "name": "z-lint", "web_url": "https://gl/j/2", "allow_failure": false },
                    { "name": "a-flaky", "web_url": "https://gl/j/1", "allow_failure": true },
                    { "name": "m-test", "web_url": "https://gl/j/3", "allow_failure": false },
                ]),
            );
        }
        res(404, Value::Null)
    });
    let status = read(&lab).await.status.unwrap();
    assert_eq!(status.phase, PrPhase::Open);
    // The author's own approval does not count; a requested change does.
    assert_eq!(status.review, PrReview::ChangesRequested);
    assert_eq!(status.checks, PrChecks::Red);
    let failing = status.failing.unwrap();
    assert_eq!(
        (failing.name.as_str(), failing.url.as_str()),
        ("m-test", "https://gl/j/3")
    );
}

#[tokio::test]
async fn pipelines_map_to_checks() {
    let checks = |status: Option<&str>| {
        let status = status.map(str::to_owned);
        async move {
            let lab = with_mr(
                mr(|v| {
                    if let Some(s) = &status {
                        v["head_pipeline"] = json!({ "id": 1, "status": s });
                    }
                }),
                Some("main"),
            );
            read(&lab).await.status.unwrap().checks
        }
    };
    assert_eq!(checks(None).await, PrChecks::NoChecks);
    assert_eq!(checks(Some("success")).await, PrChecks::Green);
    assert_eq!(checks(Some("running")).await, PrChecks::Pending);
    assert_eq!(checks(Some("canceled")).await, PrChecks::Pending);
    assert_eq!(checks(Some("manual")).await, PrChecks::Pending);
    assert_eq!(checks(Some("failed")).await, PrChecks::Red);
}

#[tokio::test]
async fn an_approval_by_someone_else_is_approved() {
    let lab = Lab::new(|req| {
        let p = path(req);
        if p == format!("{PROJECT}/merge_requests/7") {
            return res(200, mr(|_| {}));
        }
        if p.ends_with("/approvals") {
            return res(
                200,
                json!({ "approved_by": [{ "user": { "username": "carol" } }] }),
            );
        }
        res(404, Value::Null)
    });
    assert_eq!(read(&lab).await.status.unwrap().review, PrReview::Approved);
}

// ---- the engine ----

fn engine(lab: &Lab, tracker: &Fake, extra: Value) -> Ingest<Gitlab<Lab>, Fake> {
    let cfg = config(extra);
    let repos = cfg.repos();
    Ingest::new(
        gitlab_with(lab, &cfg),
        tracker.clone(),
        Settings {
            rules: cfg.rules.clone(),
            repos,
            reconcile_every: 1000,
            release_compares: 20,
        },
    )
}

/// A project with issue 12 and the merge request in `mr`, which a test may change.
fn project(mr: &Arc<Mutex<Value>>) -> Lab {
    let mr = mr.clone();
    Lab::new(move |req| {
        let p = path(req);
        let issue = raw(12, |_| {});
        if p == format!("{PROJECT}/issues") {
            return res(200, json!([issue]));
        }
        if p == format!("{PROJECT}/issues/12") {
            return res(200, issue);
        }
        if p == format!("{PROJECT}/merge_requests/7") {
            return res(200, mr.lock().unwrap().clone());
        }
        if p == PROJECT {
            return res(200, json!({ "default_branch": "main" }));
        }
        if p.ends_with("/notes") && req.method == "GET" {
            return res(200, json!([]));
        }
        if p.ends_with("/approvals") {
            return res(200, json!({ "approved_by": [] }));
        }
        if p.ends_with("/reviewers") {
            return res(200, json!([]));
        }
        res(200, Value::Null)
    })
}

/// Imports issue 12 and has an agent claim it and report merge request 7.
async fn reported(
    lab: &Lab,
    tracker: &Fake,
    mr: &Arc<Mutex<Value>>,
    branch: Option<&str>,
) -> (Ingest<Gitlab<Lab>, Fake>, kbl_core::names::ItemId) {
    let ingest = engine(lab, tracker, json!({}));
    ingest.poll(false).await.unwrap();
    let item = tracker.item_of("team/app#12");
    tracker.agent(
        "w1",
        WorkRecord::Claimed {
            item: item.clone(),
            attempt: 1,
            deadline_ms: 90_000,
        },
    );
    let branch = branch.map_or_else(|| pr_branch(&item), str::to_owned);
    mr.lock().unwrap()["source_branch"] = json!(branch);
    tracker.agent(
        "w1",
        WorkRecord::Progress {
            item: item.clone(),
            attempt: 1,
            note: kbl_core::content::Content::Text("fixing".into()),
            branch: Some(branch),
            pr: Some("team/app#7".into()),
        },
    );
    (ingest, item)
}

fn closure(tracker: &Fake, item: &kbl_core::names::ItemId) -> Option<kbl_core::work_fold::Closure> {
    tracker.state().items[item].closed
}

#[tokio::test]
async fn a_reported_merge_request_on_the_item_branch_closes_the_root_when_merged() {
    let mr = Arc::new(Mutex::new(merged(|_| {})));
    let lab = project(&mr);
    let tracker = Fake::default();
    let (ingest, item) = reported(&lab, &tracker, &mr, None).await;
    ingest.poll(false).await.unwrap();
    assert_eq!(
        closure(&tracker, &item),
        Some(kbl_core::work_fold::Closure::Merged)
    );
    // The comment says the request is `path!7`, which GitLab reads as a merge request.
    let writes: Vec<(String, Value)> = lab.writes();
    assert!(
        writes
            .iter()
            .any(|(w, b)| w.ends_with("/issues/12/notes")
                && b["body"]
                    .as_str()
                    .is_some_and(|b| b.starts_with("group/sub/app!7 was merged.\n\n<!-- kbl:"))),
        "{writes:?}"
    );
    assert!(
        writes.iter().any(|(w, b)| w.starts_with("PUT")
            && w.ends_with("/issues/12")
            && b["state_event"] == "close"),
        "{writes:?}"
    );
}

#[tokio::test]
async fn a_merge_is_not_recorded_for_a_request_the_agent_did_not_tie_to_the_item() {
    // A merged request on some other branch, with no closing reference.
    let mr = Arc::new(Mutex::new(merged(|v| {
        v["description"] = json!("no reference")
    })));
    let lab = project(&mr);
    let tracker = Fake::default();
    let (ingest, item) = reported(&lab, &tracker, &mr, Some("feature/other")).await;
    ingest.poll(false).await.unwrap();
    assert_eq!(closure(&tracker, &item), None);
    assert_eq!(tracker.count("pr_status"), 0);

    // The same request made before the first claim cannot be the item's work.
    let mr = Arc::new(Mutex::new(merged(|v| {
        v["created_at"] = json!("2027-01-15T07:00:00.000Z")
    })));
    let lab = project(&mr);
    let tracker = Fake::default();
    let (ingest, item) = reported(&lab, &tracker, &mr, None).await;
    ingest.poll(false).await.unwrap();
    assert_eq!(closure(&tracker, &item), None);
}

#[tokio::test]
async fn a_merge_by_us_or_into_a_side_branch_or_of_a_foreign_request_closes_nothing() {
    let cases = [
        (
            "by us",
            merged(|v| v["merge_user"] = json!({ "username": OURS })),
        ),
        ("side branch", merged(|v| v["target_branch"] = json!("dev"))),
        (
            "other author",
            merged(|v| v["author"] = json!({ "username": "mallory" })),
        ),
    ];
    for (name, request) in cases {
        let mr = Arc::new(Mutex::new(request));
        let lab = project(&mr);
        let tracker = Fake::default();
        let (ingest, item) = reported(&lab, &tracker, &mr, None).await;
        ingest.poll(false).await.unwrap();
        assert_eq!(closure(&tracker, &item), None, "{name}");
        let merged_status = tracker.state().items[&item]
            .pr_status
            .as_ref()
            .is_some_and(|s| s.phase == PrPhase::Merged);
        assert!(!merged_status, "{name}");
    }
}

#[tokio::test]
async fn a_request_in_another_project_is_not_read_for_the_item() {
    let mr = Arc::new(Mutex::new(merged(|_| {})));
    let lab = project(&mr);
    let tracker = Fake::default();
    let ingest = engine(&lab, &tracker, json!({}));
    ingest.poll(false).await.unwrap();
    let item = tracker.item_of("team/app#12");
    tracker.agent(
        "w1",
        WorkRecord::Claimed {
            item: item.clone(),
            attempt: 1,
            deadline_ms: 90_000,
        },
    );
    tracker.agent(
        "w1",
        WorkRecord::Progress {
            item: item.clone(),
            attempt: 1,
            note: kbl_core::content::Content::Text("fixing".into()),
            branch: Some(pr_branch(&item)),
            pr: Some("evil/repo#7".into()),
        },
    );
    ingest.poll(false).await.unwrap();
    assert_eq!(closure(&tracker, &item), None);
    assert!(lab.urls().iter().all(|u| !u.contains("evil")));
    assert!(lab.writes().is_empty());
}

#[tokio::test]
async fn re_polling_announces_a_request_once() {
    let mr = Arc::new(Mutex::new(mr(|_| {})));
    let lab = project(&mr);
    let tracker = Fake::default();
    let (ingest, _) = reported(&lab, &tracker, &mr, None).await;
    for _ in 0..3 {
        ingest.poll(false).await.unwrap();
    }
    assert_eq!(tracker.count("imported"), 1);
    let writes = lab.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0].0, format!("POST {PROJECT}/issues/12/notes"));
    assert_eq!(
        writes[0].1["body"],
        "An agent opened group/sub/app!7 for this issue.\n\n\
         <!-- kbl:team/app#12:linked:team/app#7 -->"
    );
}

// ---- write-back ----

#[tokio::test]
async fn a_comment_is_posted_once_and_a_state_change_only_when_needed() {
    let lab = Lab::new(|req| {
        let p = path(req);
        if req.method == "GET" && p.ends_with("/notes") {
            return res(
                200,
                json!([{ "id": 4, "body": "already said", "author": { "username": OURS },
                         "system": false, "updated_at": "2026-11-10T12:00:00.000Z" }]),
            );
        }
        if req.method == "GET" {
            return res(200, raw(12, |v| v["state"] = json!("closed")));
        }
        res(201, Value::Null)
    });
    let g = gitlab(&lab);
    let issue = IssueRef::new("team/app", 12);
    g.write_back(&WriteBack::Comment {
        mark: "<!-- kbl:t -->".into(),
        issue: issue.clone(),
        body: "already said".into(),
    })
    .await
    .unwrap();
    g.write_back(&WriteBack::Comment {
        mark: "<!-- kbl:t -->".into(),
        issue: issue.clone(),
        body: "something new".into(),
    })
    .await
    .unwrap();
    // Already closed: closing again is nothing.
    g.write_back(&WriteBack::Transition {
        issue: issue.clone(),
        name: "close".into(),
    })
    .await
    .unwrap();
    let writes = lab.writes();
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0].0, format!("POST {PROJECT}/issues/12/notes"));
    assert_eq!(writes[0].1, json!({ "body": "something new\n\n<!-- kbl:t -->" }));
}

#[tokio::test]
async fn a_close_and_a_reopen_use_the_state_event() {
    let state = Arc::new(Mutex::new("opened"));
    let st = state.clone();
    let lab = Lab::new(move |req| {
        if req.method == "GET" {
            return res(200, raw(12, |v| v["state"] = json!(*st.lock().unwrap())));
        }
        res(200, Value::Null)
    });
    let g = gitlab(&lab);
    let issue = IssueRef::new("team/app", 12);
    let go = |name: &str| WriteBack::Transition {
        issue: issue.clone(),
        name: name.into(),
    };
    g.write_back(&go("close")).await.unwrap();
    *state.lock().unwrap() = "closed";
    g.write_back(&go("reopen")).await.unwrap();
    let writes = lab.writes();
    assert_eq!(writes[0].0, format!("PUT {PROJECT}/issues/12"));
    assert_eq!(writes[0].1, json!({ "state_event": "close" }));
    assert_eq!(writes[1].1, json!({ "state_event": "reopen" }));
    // Any other name is not a GitLab state change.
    assert!(g.write_back(&go("Done")).await.is_err());
    assert_eq!(lab.writes().len(), 2);
}

#[tokio::test]
async fn nothing_is_written_unless_a_rule_asks_and_only_to_a_configured_project() {
    let lab = Lab::new(|_| res(200, raw(12, |_| {})));
    // No rule writes back: every write is refused, so a read-only token suffices.
    let cfg = config(json!({ "comment": false, "transition_on_merge": null }));
    let g = gitlab_with(&lab, &cfg);
    let issue = IssueRef::new("team/app", 12);
    for what in [
        WriteBack::Comment {
            mark: "<!-- kbl:t -->".into(),
            issue: issue.clone(),
            body: "x".into(),
        },
        WriteBack::Transition {
            issue: issue.clone(),
            name: "close".into(),
        },
        WriteBack::DraftPullRequest {
            mark: "<!-- kbl:t -->".into(),
            pr: IssueRef::new("team/app", 7),
            note: "x".into(),
        },
    ] {
        let err = g.write_back(&what).await.unwrap_err().to_string();
        assert!(err.contains("no rule asks"), "{err}");
    }
    assert!(g.open_issue("team/app", "t", "b").await.is_err());
    assert!(lab.writes().is_empty());

    // A rule that writes still reaches only the projects of its rules.
    let g = gitlab(&lab);
    let elsewhere = IssueRef::new("evil/repo", 1);
    for what in [
        WriteBack::Comment {
            mark: "<!-- kbl:t -->".into(),
            issue: elsewhere.clone(),
            body: "x".into(),
        },
        WriteBack::Transition {
            issue: elsewhere.clone(),
            name: "close".into(),
        },
        WriteBack::DraftPullRequest {
            mark: "<!-- kbl:t -->".into(),
            pr: elsewhere.clone(),
            note: "x".into(),
        },
    ] {
        assert!(g.write_back(&what).await.is_err());
    }
    assert!(g.open_issue("evil/repo", "t", "b").await.is_err());
    assert!(lab.writes().is_empty());
    assert!(lab.urls().iter().all(|u| !u.contains("evil")));
}

#[tokio::test]
async fn a_dropped_issue_makes_our_open_request_a_draft_and_leaves_others_alone() {
    let request = Arc::new(Mutex::new(mr(|_| {})));
    let r = request.clone();
    let lab = Lab::new(move |req| {
        if req.method == "GET" {
            return res(200, r.lock().unwrap().clone());
        }
        res(200, Value::Null)
    });
    let g = gitlab(&lab);
    let draft = || WriteBack::DraftPullRequest {
        mark: "<!-- kbl:t -->".into(),
        pr: IssueRef::new("team/app", 7),
        note: "dropped".into(),
    };
    g.write_back(&draft()).await.unwrap();
    let writes = lab.writes();
    assert_eq!(writes[0].0, format!("PUT {PROJECT}/merge_requests/7"));
    assert_eq!(writes[0].1, json!({ "title": "Draft: Fix the stale read" }));
    assert_eq!(
        writes[1].0,
        format!("POST {PROJECT}/merge_requests/7/notes")
    );
    // Someone else's request, or one already merged, is not touched.
    for edit in [
        mr(|v| v["author"] = json!({ "username": "mallory" })),
        merged(|_| {}),
    ] {
        *request.lock().unwrap() = edit;
        g.write_back(&draft()).await.unwrap();
    }
    assert_eq!(lab.writes().len(), 2);
}

#[tokio::test]
async fn a_failed_write_is_an_error_without_the_answer() {
    let lab = Lab::new(|req| {
        if req.method == "GET" {
            return res(200, json!([]));
        }
        res(403, json!({ "message": "403 Forbidden - secret-detail" }))
    });
    let err = gitlab(&lab)
        .write_back(&WriteBack::Comment {
            mark: "<!-- kbl:t -->".into(),
            issue: IssueRef::new("team/app", 12),
            body: "x".into(),
        })
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("403") && !err.contains("secret-detail"),
        "{err}"
    );
}

#[tokio::test]
async fn an_issue_is_opened_only_in_a_project_a_rule_reads() {
    let lab = Lab::new(|_| res(201, json!({ "iid": 31 })));
    let cfg = config(
        json!({ "comment": false, "transition_on_merge": null, "open_prerequisites": true }),
    );
    let g = gitlab_with(&lab, &cfg);
    let id = g.open_issue("team/app", "Title", "Body").await.unwrap();
    assert_eq!(id.to_string(), "team/app#31");
    let writes = lab.writes();
    assert_eq!(writes[0].0, format!("POST {PROJECT}/issues"));
    assert_eq!(
        writes[0].1,
        json!({ "title": "Title", "description": "Body" })
    );
    assert!(g.open_issue("other/proj", "t", "b").await.is_err());
}

// ---- sign-in, limits and the address ----

#[tokio::test]
async fn a_failed_sign_in_is_a_denial_not_an_import() {
    for status in [401, 403, 404, 302] {
        let lab = Lab::new(move |_| res(status, Value::Null));
        let changes = gitlab(&lab).changed(&repos(), &none()).await.unwrap();
        assert!(changes.issues.is_empty());
        assert!(changes.cursors.is_empty(), "{status}");
        let want = if status == 302 { 401 } else { status };
        assert_eq!(changes.denied["team/app"], want);
    }
}

#[tokio::test]
async fn a_server_error_fails_the_poll_and_says_only_the_status() {
    let lab = Lab::new(|_| res(502, json!({ "message": "token glpat-abc was rejected" })));
    let err = gitlab(&lab)
        .changed(&repos(), &none())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(err, "GitLab answered 502");
}

#[tokio::test]
async fn a_rate_limit_waits_and_holds_every_call_until_it_passes() {
    let lab = Lab::new(|_| {
        let mut r = res(429, Value::Null);
        r.headers.insert("retry-after".into(), "120".into());
        r
    });
    let g = gitlab(&lab);
    let err = g.changed(&repos(), &none()).await.unwrap_err();
    assert!(matches!(err, SourceError::RateLimited { wait } if wait.as_secs() == 120));
    let sent = lab.urls().len();
    assert!(matches!(
        g.changed(&repos(), &none()).await,
        Err(SourceError::RateLimited { .. })
    ));
    assert_eq!(lab.urls().len(), sent);
}

#[tokio::test]
async fn a_deleted_issue_is_gone() {
    let lab = Lab::new(|_| res(404, Value::Null));
    assert_eq!(
        gitlab(&lab)
            .fetch(&IssueRef::new("team/app", 12))
            .await
            .unwrap(),
        Fetched::Gone
    );
    let lab = listing(vec![raw(12, |_| {})]);
    assert!(matches!(
        gitlab(&lab)
            .fetch(&IssueRef::new("team/app", 12))
            .await
            .unwrap(),
        Fetched::Issue(_)
    ));
}

#[test]
fn the_address_is_https_or_loopback_and_carries_nothing_else() {
    for ok in [
        "https://gitlab.com",
        "https://gitlab.example.com/gitlab/",
        "http://localhost:8080",
        "http://127.0.0.1:3000",
        "http://[::1]:3000",
    ] {
        assert!(base_url(ok).is_ok(), "{ok}");
    }
    for bad in [
        "http://gitlab.example.com",
        "http://localhost.example.com",
        "http://10.0.0.5",
        "ftp://gitlab.example.com",
        "https://user:pw@gitlab.example.com",
        "https://token@gitlab.example.com",
        "https://gitlab.example.com/?x=1",
        "https://gitlab.example.com/#x",
        "gitlab.example.com",
    ] {
        let err = base_url(bad).unwrap_err().to_string();
        // The address, which may hold a password, is not in the message.
        assert!(
            !err.contains("pw") && !err.contains("example.com"),
            "{bad}: {err}"
        );
    }
    let lab = Lab::new(|_| res(200, Value::Null));
    let cfg = config(json!({}));
    let g = cfg.gitlab.as_ref().unwrap();
    assert!(
        Gitlab::new(
            "http://gitlab.example.com",
            lab,
            vec![],
            cfg.rules.clone(),
            &g.projects
        )
        .is_err()
    );
}

#[tokio::test]
async fn a_self_managed_path_prefix_is_kept() {
    let lab = listing(vec![]);
    let cfg = config(json!({}));
    let g = cfg.gitlab.as_ref().unwrap();
    let gl = Gitlab::new(
        "https://git.corp.example/gitlab/",
        lab.clone(),
        vec![OURS.into()],
        cfg.rules.clone(),
        &g.projects,
    )
    .unwrap();
    gl.changed(&repos(), &none()).await.unwrap();
    assert!(
        lab.urls()[0].starts_with(
            "https://git.corp.example/gitlab/api/v4/projects/group%2Fsub%2Fapp/issues?"
        ),
        "{}",
        lab.urls()[0]
    );
}

#[test]
fn an_unknown_credential_with_no_own_logins_fails_startup() {
    let lab = Lab::new(|_| res(403, Value::Null));
    let cfg = config(json!({}));
    let g = cfg.gitlab.as_ref().unwrap();
    let mut gl = Gitlab::new(&g.api, lab, vec![], cfg.rules.clone(), &g.projects).unwrap();
    let err = gl.require_own().unwrap_err().to_string();
    assert!(err.contains("own_logins"), "{err}");
    gl.add_own(vec!["kbl-bot".into()]);
    assert!(gl.require_own().is_ok());
}

#[tokio::test]
async fn the_credential_names_itself_through_the_user_endpoint() {
    let lab = Lab::new(|req| {
        assert_eq!(path(req), "/api/v4/user");
        res(
            200,
            json!({ "username": "project_5_bot_abc", "name": "KBL" }),
        )
    });
    assert_eq!(gitlab(&lab).login().await, ["project_5_bot_abc"]);
}

#[tokio::test]
async fn releases_and_tags_come_from_the_release_and_ref_listings() {
    let lab = Lab::new(|req| {
        let p = path(req);
        if p == format!("{PROJECT}/releases") {
            return res(
                200,
                json!([
                    { "tag_name": "v1.2.0", "released_at": "2027-02-01T00:00:00.000Z" },
                    { "tag_name": "v1.3.0", "released_at": "2099-01-01T00:00:00.000Z",
                      "upcoming_release": true },
                    { "tag_name": "v1.1.0", "released_at": "2027-01-10T00:00:00.000Z" },
                    { "tag_name": "../bad", "released_at": "2027-01-11T00:00:00.000Z" },
                ]),
            );
        }
        if p.ends_with("/repository/commits/abc123/refs") {
            assert_eq!(param(req, "type").as_deref(), Some("tag"));
            return res(200, json!([{ "type": "tag", "name": "v1.2.0" }]));
        }
        res(404, Value::Null)
    });
    let g = gitlab(&lab);
    let out = g.releases("team/app", 0).await.unwrap();
    let tags: Vec<&str> = out.list.iter().map(|r| r.tag.as_str()).collect();
    assert_eq!(tags, ["v1.1.0", "v1.2.0"]);
    assert!(out.whole);
    assert!(g.contains("team/app", "v1.2.0", "abc123").await.unwrap());
    assert!(!g.contains("team/app", "v1.1.0", "abc123").await.unwrap());
    // A value that is not a plain ref never reaches an address.
    let sent = lab.urls().len();
    assert!(!g.contains("team/app", "../x", "abc123").await.unwrap());
    assert_eq!(lab.urls().len(), sent);
}

#[test]
fn times_are_read_in_utc_only() {
    assert_eq!(utc_ms("2027-01-15T08:00:00Z"), Some(1_800_000_000_000));
    assert_eq!(utc_ms("2027-01-15T08:00:00.5Z"), Some(1_800_000_000_500));
    assert_eq!(
        utc_ms("2027-01-15T08:00:00.123456Z"),
        Some(1_800_000_000_123)
    );
    assert_eq!(utc_ms("2027-01-15T09:00:00.000+01:00"), None);
    assert_eq!(utc_ms("2027-01-15T08:00:00.Z"), None);
    assert_eq!(utc_ms("nonsense"), None);
}

// ---- the token ----

/// Serves one request on loopback and returns what it received.
async fn serve_once(
    status: &'static str,
    extra: String,
) -> (String, tokio::task::JoinHandle<String>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut seen = Vec::new();
        let mut buf = [0u8; 1024];
        while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = sock.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            seen.extend_from_slice(&buf[..n]);
        }
        let reply = format!(
            "HTTP/1.1 {status}\r\n{extra}content-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{{}}"
        );
        sock.write_all(reply.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&seen).into_owned()
    });
    (format!("http://{addr}/api/v4/user"), handle)
}

fn token_file(name: &str, content: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("kbl-gitlab-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("token");
    std::fs::write(&p, content).unwrap();
    p
}

#[tokio::test]
async fn the_token_goes_only_in_the_private_token_header() {
    let (url, request) = serve_once("401 Unauthorized", String::new()).await;
    let file = token_file("header", "  glpat-s3cret-value\n");
    let http = transport::Reqwest::new(Secret::File(file)).unwrap();
    let got = http.send(Req::get(url.clone())).await.unwrap();
    assert_eq!(got.status, 401);
    let seen = request.await.unwrap().to_ascii_lowercase();
    assert!(
        seen.contains("private-token: glpat-s3cret-value\r\n"),
        "{seen}"
    );
    assert!(!seen.contains("authorization"), "{seen}");
    // Not in the address either.
    assert!(!url.contains("s3cret"));
}

#[tokio::test]
async fn the_token_is_not_in_an_error() {
    // A value no header can hold.
    let file = token_file("bad", "glpat-s3cret\u{7f}value");
    let http = transport::Reqwest::new(Secret::File(file.clone())).unwrap();
    let err = format!(
        "{:#}",
        http.send(Req::get("http://127.0.0.1:1/".into()))
            .await
            .unwrap_err()
    );
    assert!(!err.contains("s3cret"), "{err}");
    // An empty file, a missing one, and a closed port.
    let empty = token_file("empty", "\n");
    let http = transport::Reqwest::new(Secret::File(empty)).unwrap();
    let err = format!(
        "{:#}",
        http.send(Req::get("http://127.0.0.1:1/".into()))
            .await
            .unwrap_err()
    );
    assert!(err.contains("empty"), "{err}");
    let missing = std::path::PathBuf::from("/nonexistent/glpat-s3cret");
    let http = transport::Reqwest::new(Secret::File(missing)).unwrap();
    assert!(
        http.send(Req::get("http://127.0.0.1:1/".into()))
            .await
            .is_err()
    );
    let good = token_file("refused", "glpat-s3cret-value");
    let http = transport::Reqwest::new(Secret::File(good)).unwrap();
    let err = format!(
        "{:#}",
        http.send(Req::get("http://127.0.0.1:1/x".into()))
            .await
            .unwrap_err()
    );
    assert!(!err.contains("s3cret"), "{err}");
}

#[tokio::test]
async fn a_redirect_is_not_followed_with_the_token() {
    let elsewhere = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let to = format!("location: http://{}/\r\n", elsewhere.local_addr().unwrap());
    let (url, request) = serve_once("302 Found", to).await;
    let file = token_file("redirect", "glpat-s3cret-value");
    let http = transport::Reqwest::new(Secret::File(file)).unwrap();
    let got = http.send(Req::get(url)).await.unwrap();
    assert_eq!(got.status, 302);
    request.await.unwrap();
    let tried = tokio::time::timeout(Duration::from_millis(300), elsewhere.accept()).await;
    assert!(tried.is_err(), "the redirect target was contacted");
}

#[tokio::test]
async fn a_full_page_with_no_next_page_header_is_followed() {
    let lab = Lab::new(|req| {
        let page = param(req, "page").unwrap();
        let n = if page == "1" { 100 } else { 1 };
        res(
            200,
            Value::Array(
                (0..n)
                    .map(|i| raw(1000 * page.parse::<u64>().unwrap() + i, |_| {}))
                    .collect(),
            ),
        )
    });
    let changes = gitlab(&lab).changed(&repos(), &none()).await.unwrap();
    assert_eq!(changes.issues.len(), 101);
}

#[test]
fn a_merge_request_reference_is_rewritten_ignoring_case_and_only_at_a_word() {
    assert_eq!(
        mr_refs(
            "opened Team/App#7 and other/team/app#8 (team/app#9)",
            "team/app",
            "g/s/app"
        ),
        "opened g/s/app!7 and other/team/app#8 (g/s/app!9)"
    );
}

#[tokio::test]
async fn an_invented_request_number_is_not_announced() {
    let mr = Arc::new(Mutex::new(mr(|_| {})));
    let lab = project(&mr);
    let tracker = Fake::default();
    let (ingest, item) = reported(&lab, &tracker, &mr, None).await;
    // The request is a stranger's: nothing is read as ours, so nothing is said.
    *mr.lock().unwrap() = self::mr(|v| v["author"] = json!({ "username": "mallory" }));
    let _ = item;
    ingest.poll(false).await.unwrap();
    assert!(lab.writes().is_empty(), "{:?}", lab.writes());
}

#[tokio::test]
async fn a_draft_note_whose_post_may_have_landed_is_not_posted_again() {
    let mark = "<!-- kbl:team/app#12:draft:team/app#7 -->";
    let notes = Arc::new(Mutex::new(json!([])));
    let n = notes.clone();
    let lab = Lab::new(move |req| match req.method {
        "GET" if req.url.contains("/notes") => res(200, n.lock().unwrap().clone()),
        "GET" => res(200, mr(|v| v["draft"] = json!(true))),
        _ => res(201, Value::Null),
    });
    let g = gitlab(&lab);
    let draft = WriteBack::DraftPullRequest {
        mark: mark.into(),
        pr: IssueRef::new("team/app", 7),
        note: "dropped".into(),
    };
    g.write_back(&draft).await.unwrap();
    assert_eq!(lab.writes()[0].1["body"], format!("dropped\n\n{mark}"));
    // The post was cut short but landed: the retry finds its mark.
    *notes.lock().unwrap() = json!([{
        "id": 1,
        "author": { "username": OURS },
        "body": format!("dropped\n\n{mark}"),
    }]);
    g.write_back(&draft).await.unwrap();
    assert_eq!(lab.writes().len(), 1);
}
