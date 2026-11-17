use std::sync::{Arc, Mutex};

use kbl_core::work_fold::NeedsYou;
use serde_json::{Value, json};

use super::*;
use crate::config::Config;
use crate::sync::{Ingest, Settings};
use crate::testing::Fake;

type Handler = dyn Fn(&Req) -> Res + Send + Sync;

/// Jira as hand-written: a handler answers each request and every request is kept.
#[derive(Clone)]
struct Site {
    handler: Arc<Handler>,
    sent: Arc<Mutex<Vec<Req>>>,
}

impl Site {
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
                let path = r.url.split("/rest/api/2").nth(1).unwrap().to_owned();
                (path, r.body.clone().unwrap_or(Value::Null))
            })
            .collect()
    }
}

impl Transport for Site {
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

fn jql_of(req: &Req) -> String {
    reqwest::Url::parse(&req.url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "jql")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

fn raw(key: &str, edit: impl FnOnce(&mut Value)) -> Value {
    let mut v = json!({
        "key": key,
        "self": "https://acme.atlassian.net/rest/api/2/issue/10001",
        "fields": {
            "summary": "Stale read after failover",
            "description": "Reads go stale.",
            "status": { "name": "To Do", "statusCategory": { "key": "new" } },
            "resolution": null,
            "labels": ["agent-ok"],
            "priority": { "name": "High" },
            "components": [{ "name": "storage" }],
            "issuetype": { "name": "Bug" },
            "reporter": { "displayName": "Alice" },
            "assignee": { "displayName": "Carol" },
            "fixVersions": [{ "name": "0.8" }],
            "updated": "2026-11-10T12:00:05.000+0000"
        }
    });
    edit(&mut v);
    v
}

fn scopes() -> Vec<Scope> {
    vec![Scope {
        name: "agent-ok".into(),
        repo: "acme/FEL".into(),
        jql: "labels = agent-ok".into(),
    }]
}

fn jira(site: &Site) -> Jira<Site> {
    Jira::new(
        "acme",
        "https://acme.atlassian.net/",
        site.clone(),
        vec!["KBL".into()],
        scopes(),
        "https://github.com",
    )
}

/// A site with one open issue, FEL-12, selected by the rule's filter.
fn listing(issues: Vec<Value>) -> Site {
    Site::new(move |req| {
        if req.url.contains("/search/jql") {
            let keys: Vec<Value> = if jql_of(req).contains("labels = agent-ok") {
                issues.iter().map(|i| json!({ "key": i["key"] })).collect()
            } else {
                issues.clone()
            };
            return res(200, json!({ "issues": keys, "isLast": true }));
        }
        if req.url.ends_with("/transitions") && req.method == "GET" {
            return res(
                200,
                json!({ "transitions": [{ "id": "21", "name": "In Review", "to": { "name": "In Review" } }] }),
            );
        }
        if req.method == "GET" {
            return res(200, json!({ "comments": [] }));
        }
        res(201, Value::Null)
    })
}

fn none() -> BTreeMap<String, Cursor> {
    BTreeMap::new()
}

fn repos() -> Vec<String> {
    vec!["acme/FEL".to_owned()]
}

#[tokio::test]
async fn fields_map_and_the_jql_filter_decides_the_scope() {
    let site = listing(vec![
        raw("FEL-12", |_| {}),
        raw("FEL-13", |v| {
            v["fields"]["status"]["statusCategory"]["key"] = json!("done");
            v["fields"]["resolution"] = json!({ "name": "Won't Do" });
        }),
    ]);
    let changes = jira(&site).changed(&repos(), &none()).await.unwrap();
    let a = &changes.issues[0];
    assert_eq!(a.id.to_string(), "acme/FEL#12");
    assert_eq!(a.title, "Stale read after failover");
    assert_eq!(a.labels, ["agent-ok"]);
    assert_eq!(a.priority.as_deref(), Some("High"));
    assert_eq!(a.components, ["storage"]);
    assert_eq!(a.kind.as_deref(), Some("Bug"));
    assert_eq!(a.milestone.as_deref(), Some("0.8"));
    assert_eq!(a.author, "Alice");
    assert_eq!(a.assignees, ["Carol"]);
    assert_eq!(a.url, "https://acme.atlassian.net/browse/FEL-12");
    assert_eq!(a.scopes, ["agent-ok"]);
    assert_eq!(changes.issues[1].closed, Some(Closed::Dropped));
    // The first poll lists open issues, then the cursor is the newest update.
    assert!(jql_of(&site.sent.lock().unwrap()[0]).contains("statusCategory != Done"));
    assert_eq!(
        changes.cursors["acme/FEL"].issues_since.as_deref(),
        Some("2026-11-10T12:00:05.000+0000")
    );
}

#[tokio::test]
async fn an_unselected_issue_has_no_scope_and_a_resolved_one_is_completed() {
    let site = Site::new(|req| {
        if jql_of(req).contains("labels = agent-ok") {
            return res(200, json!({ "issues": [], "isLast": true }));
        }
        let done = raw("FEL-1", |v| {
            v["fields"]["status"]["statusCategory"]["key"] = json!("done");
            v["fields"]["resolution"] = json!({ "name": "Fixed" });
        });
        res(200, json!({ "issues": [done], "isLast": true }))
    });
    let changes = jira(&site).changed(&repos(), &none()).await.unwrap();
    assert!(changes.issues[0].scopes.is_empty());
    assert_eq!(changes.issues[0].closed, Some(Closed::Completed));
}

#[tokio::test]
async fn paging_follows_the_token_and_a_later_poll_asks_since_the_cursor() {
    let site = Site::new(|req| {
        let q: BTreeMap<String, String> = reqwest::Url::parse(&req.url)
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if !q["jql"].contains("labels") {
            return match q.get("nextPageToken").map(String::as_str) {
                None => res(
                    200,
                    json!({ "issues": [raw("FEL-1", |_| {})], "nextPageToken": "t2" }),
                ),
                Some(_) => res(
                    200,
                    json!({ "issues": [raw("FEL-2", |v| v["fields"]["updated"] = json!("2026-11-11T08:30:00.000+0000"))], "isLast": true }),
                ),
            };
        }
        res(200, json!({ "issues": [], "isLast": true }))
    });
    let before = Cursor {
        issues_since: Some("2026-11-01T09:15:42.000+0000".into()),
        ..Cursor::default()
    };
    let cursors = BTreeMap::from([("acme/FEL".to_owned(), before)]);
    let changes = jira(&site).changed(&repos(), &cursors).await.unwrap();
    assert_eq!(changes.issues.len(), 2);
    assert_eq!(
        changes.cursors["acme/FEL"].issues_since.as_deref(),
        Some("2026-11-11T08:30:00.000+0000")
    );
    let first = jql_of(&site.sent.lock().unwrap()[0]);
    assert!(first.contains("updated >= \"2026/11/01 09:15\""), "{first}");
    assert!(!first.contains("statusCategory"));
}

#[tokio::test]
async fn a_rate_limit_waits_and_holds_every_call_until_it_passes() {
    let site = Site::new(|_| {
        let mut r = res(429, Value::Null);
        r.headers.insert("retry-after".into(), "7".into());
        r
    });
    let j = jira(&site);
    let Err(SourceError::RateLimited { wait }) = j.changed(&repos(), &none()).await else {
        panic!("expected a rate limit");
    };
    assert_eq!(wait, Duration::from_secs(7));
    // The wait is remembered: no second request goes out inside it.
    assert!(j.changed(&repos(), &none()).await.is_err());
    assert_eq!(site.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_unreadable_project_is_denied_without_moving_its_cursor() {
    let site = Site::new(|_| res(404, Value::Null));
    let changes = jira(&site).changed(&repos(), &none()).await.unwrap();
    assert_eq!(changes.denied["acme/FEL"], 404);
    assert!(changes.cursors.is_empty());
}

#[tokio::test]
async fn blocks_links_and_sub_tasks_become_edges() {
    let site = Site::new(|_| {
        res(
            200,
            json!({ "fields": {
                "issuelinks": [
                    { "type": { "name": "Blocks" }, "inwardIssue": { "key": "FEL-3" } },
                    { "type": { "name": "Blocks" }, "outwardIssue": { "key": "FEL-9" } },
                    { "type": { "name": "Relates" }, "inwardIssue": { "key": "FEL-4" } }
                ],
                "subtasks": [{ "key": "FEL-13" }]
            } }),
        )
    });
    let links = jira(&site)
        .links(&IssueRef::new("acme/FEL", 12))
        .await
        .unwrap();
    // The inward issue of Blocks is the blocker; the outward one waits on this.
    assert_eq!(links.blocked_by, [IssueRef::new("acme/FEL", 3)]);
    assert_eq!(links.sub_issues, [IssueRef::new("acme/FEL", 13)]);
    assert!(links.complete);
}

#[tokio::test]
async fn a_moved_issue_is_followed_and_a_deleted_one_is_gone() {
    let site = Site::new(|req| {
        if req.url.contains("/issue/FEL-12") {
            res(200, raw("OPS-5", |_| {}))
        } else {
            res(404, Value::Null)
        }
    });
    let j = jira(&site);
    assert_eq!(
        j.fetch(&IssueRef::new("acme/FEL", 12)).await.unwrap(),
        Fetched::Moved(IssueRef::new("acme/OPS", 5))
    );
    assert_eq!(
        j.fetch(&IssueRef::new("acme/FEL", 99)).await.unwrap(),
        Fetched::Gone
    );
}

#[tokio::test]
async fn write_back_posts_a_comment_a_remote_link_and_a_transition() {
    let site = Site::new(|req| {
        if req.url.ends_with("/transitions") && req.method == "GET" {
            return res(
                200,
                json!({ "transitions": [
                    { "id": "21", "name": "Start review", "to": { "name": "In Review" } },
                    { "id": "31", "name": "Finish", "to": { "name": "Done" } }
                ] }),
            );
        }
        if req.method == "GET" {
            return res(200, json!({ "comments": [] }));
        }
        res(201, Value::Null)
    });
    let j = jira(&site);
    let issue = IssueRef::new("acme/FEL", 12);
    let pr = IssueRef::new("GetFelix/felix", 40);
    j.write_back(&WriteBack::LinkPullRequest {
        issue: issue.clone(),
        pr: pr.clone(),
    })
    .await
    .unwrap();
    j.write_back(&WriteBack::Comment {
        issue: issue.clone(),
        body: "An agent opened GetFelix/felix#40 for this issue.".into(),
    })
    .await
    .unwrap();
    j.write_back(&WriteBack::Transition {
        issue: issue.clone(),
        name: "in review".into(),
    })
    .await
    .unwrap();
    // A transition the workflow does not offer is skipped, not an error.
    j.write_back(&WriteBack::Transition {
        issue,
        name: "Reopen".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        site.writes(),
        [
            (
                "/issue/FEL-12/remotelink".to_owned(),
                json!({
                    "globalId": "kbl-pr:GetFelix/felix#40",
                    "object": {
                        "url": "https://github.com/GetFelix/felix/pull/40",
                        "title": "Pull request GetFelix/felix#40"
                    }
                })
            ),
            (
                "/issue/FEL-12/comment".to_owned(),
                json!({ "body": "An agent opened GetFelix/felix#40 for this issue." })
            ),
            (
                "/issue/FEL-12/transitions".to_owned(),
                json!({ "transition": { "id": "21" } })
            ),
        ]
    );
}

#[tokio::test]
async fn a_comment_already_there_is_not_posted_again() {
    let site = Site::new(|req| {
        if req.method == "GET" {
            return res(
                200,
                json!({ "comments": [{ "id": "7", "author": { "displayName": "KBL" }, "body": "hi" }] }),
            );
        }
        res(201, Value::Null)
    });
    jira(&site)
        .write_back(&WriteBack::Comment {
            issue: IssueRef::new("acme/FEL", 12),
            body: "hi".into(),
        })
        .await
        .unwrap();
    assert!(site.writes().is_empty());
}

fn config(extra: Value) -> Config {
    let mut rule = json!({
        "name": "agent-ok", "repo": "acme/FEL", "jql": "labels = agent-ok",
        "mode": "admit", "components": { "storage": "GetFelix/felix" },
        "comment": true, "link_pr": true, "transition_on_pr": "In Review"
    });
    rule.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    Config::parse(
        &json!({
            "tracker": { "url": "http://t", "token_env": "T" },
            "jira": {
                "site": "acme", "api": "https://acme.atlassian.net",
                "email": "bot@acme.test", "token_env": "J"
            },
            "rules": [rule],
        })
        .to_string(),
    )
    .unwrap()
}

fn engine(site: &Site, tracker: &Fake, extra: Value) -> Ingest<Jira<Site>, Fake> {
    let cfg = config(extra);
    let repos = cfg.repos();
    Ingest::new(
        jira(site),
        tracker.clone(),
        Settings {
            rules: cfg.rules,
            repos,
            reconcile_every: 1000,
            release_compares: 20,
        },
    )
}

#[tokio::test]
async fn re_polling_never_imports_an_issue_twice() {
    let site = listing(vec![raw("FEL-12", |_| {})]);
    let tracker = Fake::default();
    let ingest = engine(&site, &tracker, json!({}));
    for _ in 0..3 {
        ingest.poll(false).await.unwrap();
    }
    assert_eq!(tracker.count("imported"), 1);
    assert_eq!(tracker.count("accepted"), 1);
    // A lost cursor re-lists everything and still imports nothing new.
    tracker.forget_saved();
    ingest.poll(false).await.unwrap();
    assert_eq!(tracker.count("imported"), 1);
    let item = tracker.item_of("acme/FEL#12");
    assert_eq!(tracker.state().needs_you(&item), None);
}

#[tokio::test]
async fn an_issue_with_no_mapped_repository_goes_to_needs_you() {
    let site = listing(vec![raw("FEL-12", |v| {
        v["fields"]["components"] = json!([{ "name": "docs" }]);
    })]);
    let tracker = Fake::default();
    let ingest = engine(&site, &tracker, json!({}));
    ingest.poll(false).await.unwrap();
    let item = tracker.item_of("acme/FEL#12");
    assert_eq!(tracker.state().needs_you(&item), Some(NeedsYou::LeftScope));
    // Once a mapping covers it, the next poll brings it back.
    tracker.person(kbl_core::work::WorkRecord::FormatRaised { to: 2 });
    let mapped = engine(&site, &tracker, json!({ "target_repo": "GetFelix/felix" }));
    mapped.poll(false).await.unwrap();
    assert_eq!(tracker.state().needs_you(&item), None);
}

#[tokio::test]
async fn an_agents_pull_request_is_announced_once() {
    let site = listing(vec![raw("FEL-12", |_| {})]);
    let tracker = Fake::default();
    let ingest = engine(&site, &tracker, json!({}));
    ingest.poll(false).await.unwrap();
    let item = tracker.item_of("acme/FEL#12");
    tracker.agent(
        "w1",
        kbl_core::work::WorkRecord::Claimed {
            item: item.clone(),
            attempt: 1,
            deadline_ms: 90_000,
        },
    );
    tracker.agent(
        "w1",
        kbl_core::work::WorkRecord::Progress {
            item,
            attempt: 1,
            note: kbl_core::content::Content::Text("fixing".into()),
            branch: Some("fix/x".into()),
            pr: Some("GetFelix/felix#40".into()),
        },
    );
    for _ in 0..3 {
        ingest.poll(false).await.unwrap();
    }
    let paths: Vec<String> = site.writes().into_iter().map(|w| w.0).collect();
    assert_eq!(
        paths,
        [
            "/issue/FEL-12/remotelink",
            "/issue/FEL-12/comment",
            "/issue/FEL-12/transitions"
        ]
    );
}

#[test]
fn priorities_map_to_tiers_with_defaults_and_overrides() {
    let issue = |p: &str| {
        let mut i = Jira::new(
            "acme",
            "https://x",
            Site::new(|_| res(200, Value::Null)),
            vec![],
            vec![],
            "",
        )
        .parse(&raw("FEL-1", |v| {
            v["fields"]["priority"]["name"] = json!(p)
        }))
        .unwrap();
        i.scopes = vec!["agent-ok".into()];
        i
    };
    let plain = config(json!({})).rules.remove(0);
    let tiers: Vec<u8> = ["Highest", "High", "Medium", "Low", "Lowest", "Odd"]
        .iter()
        .map(|p| plain.tier_for(&issue(p)))
        .collect();
    // An unknown name takes the rule's default tier.
    assert_eq!(tiers, [0, 1, 2, 3, 3, 2]);
    let custom = config(json!({ "priority_labels": { "High": 0 } }))
        .rules
        .remove(0);
    assert_eq!(custom.tier_for(&issue("High")), 0);
}

/// The real transport against a local server: the credential goes out as the
/// right header, and a 429 carries its wait.
#[tokio::test]
async fn the_transport_signs_requests_and_reads_the_retry_wait() {
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use transport::{Auth, Reqwest, Secret};

    let app = axum::Router::new()
        .route(
            "/who",
            get(|h: HeaderMap| async move {
                axum::Json(json!({ "auth": h["authorization"].to_str().unwrap() }))
            }),
        )
        .route(
            "/limited",
            post(|| async { (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "9")], "") }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = std::env::temp_dir().join(format!("kbl-jira-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("token");
    std::fs::write(&file, "s3cret\n").unwrap();

    let basic = Reqwest::new(Auth::Token {
        email: "bot@acme.test".into(),
        secret: Secret::File(file.clone()),
    })
    .unwrap();
    let got = basic.send(Req::get(format!("{base}/who"))).await.unwrap();
    // base64 of "bot@acme.test:s3cret"
    assert_eq!(got.body["auth"], "Basic Ym90QGFjbWUudGVzdDpzM2NyZXQ=");

    let bearer = Reqwest::new(Auth::Bearer(Secret::File(file))).unwrap();
    let got = bearer.send(Req::get(format!("{base}/who"))).await.unwrap();
    assert_eq!(got.body["auth"], "Bearer s3cret");

    let limited = bearer
        .send(Req::post(format!("{base}/limited"), json!({})))
        .await
        .unwrap();
    assert_eq!(
        (limited.status, limited.headers["retry-after"].as_str()),
        (429, "9")
    );
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn a_failed_rule_search_stalls_the_project_and_keeps_its_scopes() {
    let site = Site::new(|req| {
        if req.url.contains("/search/jql") {
            if jql_of(req).contains("labels = agent-ok") {
                return res(401, Value::Null);
            }
            return res(
                200,
                json!({ "issues": [raw("FEL-12", |_| {})], "isLast": true }),
            );
        }
        res(200, json!({ "comments": [] }))
    });
    let changes = jira(&site).changed(&repos(), &none()).await.unwrap();
    assert_eq!(changes.denied["acme/FEL"], 401);
    // No issue is reported without its scopes, and the cursor stays put.
    assert!(changes.issues.is_empty());
    assert!(changes.cursors.is_empty());
}

#[test]
fn an_unknown_credential_with_no_own_logins_fails_startup() {
    let site = Site::new(|_| res(403, Value::Null));
    let mut j = jira(&site);
    j.own.clear();
    let err = j.require_own().unwrap_err().to_string();
    assert!(
        err.contains("read:me") && err.contains("own_logins"),
        "{err}"
    );
    j.add_own(vec!["KBL".into()]);
    assert!(j.require_own().is_ok());
}

#[tokio::test]
async fn an_issue_link_is_built_from_the_site_and_key_not_the_self_field() {
    let site = listing(vec![raw("FEL-12", |v| {
        v["self"] = json!("https://evil.test/rest/api/2/issue/1");
    })]);
    let changes = jira(&site).changed(&repos(), &none()).await.unwrap();
    assert_eq!(
        changes.issues[0].url,
        "https://acme.atlassian.net/browse/FEL-12"
    );
}
