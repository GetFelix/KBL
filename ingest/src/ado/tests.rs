use std::sync::{Arc, Mutex};

use kbl_core::work_fold::NeedsYou;
use serde_json::{Value, json};

use super::*;
use crate::config::Config;
use crate::sync::{Ingest, Settings};
use crate::testing::Fake;

type Handler = dyn Fn(&Req) -> Res + Send + Sync;

const QUERY_ID: &str = "0b9e1d3e-7c1a-4f0e-9a52-5f2f0c0d1e11";

/// Azure DevOps as hand-written: a handler answers each request and every
/// request is kept.
#[derive(Clone)]
struct Org {
    handler: Arc<Handler>,
    sent: Arc<Mutex<Vec<Req>>>,
}

impl Org {
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
            .filter(|r| r.method != "GET" && !r.url.contains("/wiql"))
            .map(|r| {
                let u = reqwest::Url::parse(&r.url).unwrap();
                (
                    format!("{} {}", r.method, u.path()),
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

impl Transport for Org {
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

fn param(req: &Req, key: &str) -> Option<String> {
    reqwest::Url::parse(&req.url)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn path(req: &Req) -> String {
    reqwest::Url::parse(&req.url).unwrap().path().to_owned()
}

fn raw(id: u64, edit: impl FnOnce(&mut Value)) -> Value {
    let mut v = json!({
        "id": id,
        "rev": 3,
        "fields": {
            "System.TeamProject": "Platform Team",
            "System.Title": "Stale read after failover",
            "System.Description": "<div>Reads <b>go</b> stale &amp; stay.<br>Twice.</div>",
            "System.State": "Active",
            "System.Tags": "agent-ok; storage",
            "Microsoft.VSTS.Common.Priority": 1,
            "System.AreaPath": "Platform Team\\Storage\\Replication",
            "System.IterationPath": "Platform Team\\Sprint 4",
            "System.WorkItemType": "Bug",
            "System.CreatedBy": { "displayName": "Alice" },
            "System.AssignedTo": { "displayName": "Carol" },
            "System.ChangedDate": "2026-11-10T12:00:05.123Z"
        },
        "relations": []
    });
    edit(&mut v);
    v
}

fn config_json(extra: Value) -> Value {
    let mut rule = json!({
        "name": "agent-ok", "repo": "contoso/Platform", "labels_all": ["agent-ok"],
        "mode": "admit",
        "area_paths": { "Platform Team\\Storage": "GetFelix/felix" },
        "comment": true, "link_pr": true, "transition_on_pr": "Resolved"
    });
    rule.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    json!({
        "tracker": { "url": "http://t", "token_env": "T" },
        "ado": {
            "organization": "contoso",
            "projects": { "Platform": "Platform Team" },
            "pat_env": "ADO_PAT"
        },
        "rules": [rule],
    })
}

fn config(extra: Value) -> Config {
    Config::parse(&config_json(extra).to_string()).unwrap()
}

fn ado_with(org: &Org, cfg: &Config) -> Ado<Org> {
    let a = cfg.ado.as_ref().unwrap();
    let scopes = cfg
        .rules
        .iter()
        .map(|r| Scope { rule: r.clone() })
        .collect();
    Ado::new(
        &a.organization,
        "https://dev.azure.com/",
        org.clone(),
        vec!["KBL".into()],
        scopes,
        &a.projects,
        &a.pr_url_base,
    )
}

fn ado(org: &Org) -> Ado<Org> {
    ado_with(org, &config(json!({})))
}

/// An organization holding these work items, picked by the saved query when
/// `picked` lists their ids.
fn listing(items: Vec<Value>, picked: Vec<u64>) -> Org {
    Org::new(move |req| {
        let p = path(req);
        if p.ends_with("/_apis/wit/wiql") {
            let ids: Vec<Value> = items.iter().map(|i| json!({ "id": i["id"] })).collect();
            return res(200, json!({ "workItems": ids }));
        }
        if p.contains("/_apis/wit/queries/") {
            return res(200, json!({ "id": QUERY_ID }));
        }
        if p.contains(&format!("/_apis/wit/wiql/{QUERY_ID}")) {
            let ids: Vec<Value> = picked.iter().map(|i| json!({ "id": i })).collect();
            return res(200, json!({ "workItems": ids }));
        }
        if p.ends_with("/_apis/wit/workitems") {
            let want: Vec<u64> = param(req, "ids")
                .unwrap()
                .split(',')
                .filter_map(|n| n.parse().ok())
                .collect();
            let found: Vec<Value> = items
                .iter()
                .filter(|i| want.contains(&i["id"].as_u64().unwrap()))
                .cloned()
                .collect();
            return res(200, json!({ "value": found }));
        }
        if let Some(n) = p
            .rsplit_once("/_apis/wit/workitems/")
            .and_then(|(_, n)| n.parse::<u64>().ok())
        {
            return match items.iter().find(|i| i["id"] == n) {
                Some(i) => res(200, i.clone()),
                None => res(404, Value::Null),
            };
        }
        if p.ends_with("/comments") && req.method == "GET" {
            return res(200, json!({ "comments": [] }));
        }
        res(200, Value::Null)
    })
}

fn none() -> BTreeMap<String, Cursor> {
    BTreeMap::new()
}

fn repos() -> Vec<String> {
    vec!["contoso/Platform".to_owned()]
}

#[tokio::test]
async fn fields_map_and_a_tag_selects_the_item() {
    let org = listing(
        vec![
            raw(12, |_| {}),
            raw(13, |v| v["fields"]["System.State"] = json!("Removed")),
            raw(14, |v| v["fields"]["System.State"] = json!("Done")),
        ],
        vec![],
    );
    let changes = ado(&org).changed(&repos(), &none()).await.unwrap();
    let a = &changes.issues[0];
    assert_eq!(a.id.to_string(), "contoso/Platform#12");
    assert_eq!(a.title, "Stale read after failover");
    assert_eq!(a.body, "Reads go stale & stay.\nTwice.");
    assert_eq!(a.labels, ["agent-ok", "storage"]);
    assert_eq!(a.priority.as_deref(), Some("1"));
    assert_eq!(a.components, ["Platform Team\\Storage\\Replication"]);
    assert_eq!(a.kind.as_deref(), Some("Bug"));
    assert_eq!(a.author, "Alice");
    assert_eq!(a.assignees, ["Carol"]);
    assert_eq!(
        a.url,
        "https://dev.azure.com/contoso/Platform%20Team/_workitems/edit/12"
    );
    assert_eq!(a.closed, None);
    assert_eq!(changes.issues[1].closed, Some(Closed::Dropped));
    assert_eq!(changes.issues[2].closed, Some(Closed::Completed));
    // The first poll lists open items, then the cursor is the newest change.
    let first = &org.sent.lock().unwrap()[0];
    let query = first.body.as_ref().unwrap()["query"].as_str().unwrap();
    assert!(
        query.contains("NOT IN ('Done', 'Closed', 'Completed', 'Removed', 'Cut')"),
        "{query}"
    );
    assert!(
        query.contains("[System.TeamProject] = 'Platform Team'"),
        "{query}"
    );
    assert_eq!(
        changes.cursors["contoso/Platform"].issues_since.as_deref(),
        Some("2026-11-10T12:00:05.123Z")
    );
    // The tag filter is the rule's own, run here, so no rule scopes are set.
    assert!(a.scopes.is_empty());
    assert!(config(json!({})).rules[0].matches(a));
}

#[tokio::test]
async fn a_saved_query_picks_the_items_and_a_path_is_resolved_first() {
    let org = listing(vec![raw(12, |_| {}), raw(13, |_| {})], vec![12]);
    let cfg = config(json!({ "ado_query": "Shared Queries/Agent Work" }));
    let changes = ado_with(&org, &cfg)
        .changed(&repos(), &none())
        .await
        .unwrap();
    assert_eq!(changes.issues[0].scopes, ["agent-ok"]);
    assert!(changes.issues[1].scopes.is_empty());
    let urls = org.urls();
    assert!(
        urls.iter()
            .any(|u| u.contains("/Platform%20Team/_apis/wit/queries/Shared%20Queries/Agent%20Work")),
        "{urls:?}"
    );
    // The query decides, not the tags: the rule matches only the picked item.
    let rule = &cfg.rules[0];
    assert!(rule.matches(&changes.issues[0]));
    assert!(!rule.matches(&changes.issues[1]));
}

#[tokio::test]
async fn a_query_id_is_used_as_it_is() {
    let org = listing(vec![raw(12, |_| {})], vec![12]);
    let cfg = config(json!({ "ado_query": QUERY_ID }));
    ado_with(&org, &cfg)
        .changed(&repos(), &none())
        .await
        .unwrap();
    assert!(org.urls().iter().all(|u| !u.contains("/wit/queries/")));
}

#[tokio::test]
async fn a_later_poll_asks_since_the_cursor_and_a_quote_cannot_escape_the_query() {
    let org = listing(vec![raw(12, |_| {})], vec![]);
    let before = Cursor {
        issues_since: Some("2026-11-01T09:15:42.000Z".into()),
        ..Cursor::default()
    };
    let cursors = BTreeMap::from([("contoso/Platform".to_owned(), before)]);
    ado(&org).changed(&repos(), &cursors).await.unwrap();
    let q = |org: &Org| {
        org.sent.lock().unwrap()[0].body.as_ref().unwrap()["query"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let first = q(&org);
    assert!(
        first.contains("[System.ChangedDate] >= '2026-11-01T09:15:42Z'"),
        "{first}"
    );
    assert!(!first.contains("NOT IN"));

    // A cursor is saved state: one that is not a date never reaches the query.
    let org = listing(vec![], vec![]);
    let evil = Cursor {
        issues_since: Some("2026-11-01T09:15:4' OR 1=1 --".into()),
        ..Cursor::default()
    };
    let cursors = BTreeMap::from([("contoso/Platform".to_owned(), evil)]);
    ado(&org).changed(&repos(), &cursors).await.unwrap();
    let q = q(&org);
    assert!(!q.contains("OR 1=1") && q.contains("NOT IN"), "{q}");

    // A project name with a quote is doubled inside the literal.
    assert_eq!(quote("O'Brien's"), "'O''Brien''s'");
}

#[tokio::test]
async fn item_text_never_reaches_a_query_or_an_address() {
    let org = listing(
        vec![raw(12, |v| {
            v["fields"]["System.Title"] = json!("x' OR [System.Id] > 0 --");
            v["fields"]["System.Tags"] = json!("agent-ok; a'b");
            v["fields"]["System.AreaPath"] = json!("Platform Team\\Storage\\../../x");
        })],
        vec![],
    );
    let a = ado(&org);
    let changes = a.changed(&repos(), &none()).await.unwrap();
    a.links(&changes.issues[0].id).await.unwrap();
    for r in org.sent.lock().unwrap().iter() {
        let sent = format!("{} {:?}", r.url, r.body);
        assert!(
            !sent.contains("OR [System.Id]") && !sent.contains("a'b"),
            "{sent}"
        );
    }
}

#[tokio::test]
async fn predecessors_and_children_become_edges_and_foreign_projects_are_left_out() {
    let org = Org::new(|req| {
        let p = path(req);
        if p.ends_with("/workitems/12") {
            let rel = |rel: &str, n: u64| json!({ "rel": rel, "url": format!("https://dev.azure.com/contoso/_apis/wit/workItems/{n}") });
            return res(
                200,
                raw(12, |v| {
                    v["relations"] = json!([
                        rel("System.LinkTypes.Dependency-Reverse", 3),
                        rel("System.LinkTypes.Dependency-Forward", 9),
                        rel("System.LinkTypes.Hierarchy-Forward", 13),
                        rel("System.LinkTypes.Hierarchy-Reverse", 1),
                        rel("System.LinkTypes.Dependency-Reverse", 77),
                        rel("System.LinkTypes.Related", 4),
                    ]);
                }),
            );
        }
        let other = |id: u64, project: &str| json!({ "id": id, "fields": { "System.TeamProject": project } });
        res(
            200,
            json!({ "value": [
                other(3, "Platform Team"),
                other(13, "platform team"),
                other(77, "Somewhere Else")
            ] }),
        )
    });
    let links = ado(&org)
        .links(&IssueRef::new("contoso/Platform", 12))
        .await
        .unwrap();
    assert_eq!(links.blocked_by, [IssueRef::new("contoso/Platform", 3)]);
    assert_eq!(links.sub_issues, [IssueRef::new("contoso/Platform", 13)]);
    // Item 77 is in a project no rule reads, so the listing is not whole.
    assert!(!links.complete);
}

#[tokio::test]
async fn an_unreadable_link_listing_is_not_complete() {
    let org = Org::new(|_| res(500, Value::Null));
    assert!(
        ado(&org)
            .links(&IssueRef::new("contoso/Platform", 12))
            .await
            .is_err()
    );
    let org = Org::new(|_| res(404, Value::Null));
    let links = ado(&org)
        .links(&IssueRef::new("contoso/Platform", 12))
        .await
        .unwrap();
    assert!(!links.complete);
}

#[tokio::test]
async fn write_back_comments_links_and_changes_state() {
    let org = Org::new(|req| {
        if req.method == "GET" && path(req).ends_with("/comments") {
            return res(200, json!({ "comments": [] }));
        }
        if req.method == "GET" {
            return res(200, raw(12, |_| {}));
        }
        res(200, Value::Null)
    });
    let a = ado(&org);
    let issue = IssueRef::new("contoso/Platform", 12);
    let pr = IssueRef::new("GetFelix/felix", 40);
    a.write_back(&WriteBack::LinkPullRequest {
        issue: issue.clone(),
        pr: pr.clone(),
    })
    .await
    .unwrap();
    a.write_back(&WriteBack::Comment {
        mark: "<!-- kbl:t -->".into(),
        issue: issue.clone(),
        body: "An agent opened GetFelix/felix#40 <b>for</b> this & that.".into(),
    })
    .await
    .unwrap();
    a.write_back(&WriteBack::Transition {
        issue,
        name: "Resolved".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        org.writes(),
        [
            (
                "PATCH /contoso/Platform%20Team/_apis/wit/workitems/12".to_owned(),
                json!([{
                    "op": "add",
                    "path": "/relations/-",
                    "value": {
                        "rel": "Hyperlink",
                        "url": "https://github.com/GetFelix/felix/pull/40",
                        "attributes": { "comment": "Pull request GetFelix/felix#40" }
                    }
                }])
            ),
            (
                "POST /contoso/Platform%20Team/_apis/wit/workItems/12/comments".to_owned(),
                json!({ "text": "An agent opened GetFelix/felix#40 &lt;b&gt;for&lt;/b&gt; this &amp; that.<!-- kbl:t -->" })
            ),
            (
                "PATCH /contoso/Platform%20Team/_apis/wit/workitems/12".to_owned(),
                json!([{ "op": "add", "path": "/fields/System.State", "value": "Resolved" }])
            ),
        ]
    );
}

#[tokio::test]
async fn a_write_already_made_or_refused_by_the_process_is_not_an_error() {
    let url = "https://github.com/GetFelix/felix/pull/40";
    let org = Org::new(move |req| {
        if req.method == "GET" && path(req).ends_with("/comments") {
            return res(
                200,
                json!({ "comments": [{ "id": 7, "createdBy": { "displayName": "KBL" }, "text": "<div>hi</div>" }] }),
            );
        }
        if req.method == "GET" {
            return res(
                200,
                raw(12, |v| {
                    v["relations"] = json!([{ "rel": "Hyperlink", "url": url }]);
                    v["fields"]["System.State"] = json!("Active");
                }),
            );
        }
        res(
            400,
            json!({ "typeKey": "RuleValidationException", "message": "TF401320: Rule Error" }),
        )
    });
    let a = ado(&org);
    let issue = IssueRef::new("contoso/Platform", 12);
    a.write_back(&WriteBack::Comment {
        mark: "<!-- kbl:t -->".into(),
        issue: issue.clone(),
        body: "hi".into(),
    })
    .await
    .unwrap();
    a.write_back(&WriteBack::LinkPullRequest {
        issue: issue.clone(),
        pr: IssueRef::new("GetFelix/felix", 40),
    })
    .await
    .unwrap();
    assert!(org.writes().is_empty());
    // The process refuses the state from here: skipped, not retried forever.
    a.write_back(&WriteBack::Transition {
        issue,
        name: "Closed".into(),
    })
    .await
    .unwrap();
    // A refused comment is an error, so the engine retries it.
    assert!(
        a.write_back(&WriteBack::Comment {
            mark: "<!-- kbl:t -->".into(),
            issue: IssueRef::new("contoso/Platform", 12),
            body: "new".into(),
        })
        .await
        .is_err()
    );
}

#[tokio::test]
async fn nothing_is_written_unless_a_rule_asks_and_only_to_a_configured_project() {
    let org = listing(vec![], vec![]);
    let read_only = config(json!({
        "comment": false, "link_pr": false, "transition_on_pr": null
    }));
    let err = ado_with(&org, &read_only)
        .write_back(&WriteBack::Comment {
            mark: "<!-- kbl:t -->".into(),
            issue: IssueRef::new("contoso/Platform", 12),
            body: "x".into(),
        })
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no rule asks"), "{err}");
    for issue in [
        IssueRef::new("contoso/Secret", 12),
        IssueRef::new("other/Platform", 12),
    ] {
        let a = ado(&org);
        assert!(
            a.write_back(&WriteBack::Comment {
                mark: "<!-- kbl:t -->".into(),
                issue: issue.clone(),
                body: "x".into()
            })
            .await
            .is_err()
        );
        assert!(a.fetch(&issue).await.is_err());
        assert!(a.issue_comments(&issue).await.is_err());
    }
    assert!(org.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_sign_in_is_a_denial_not_an_import() {
    // A bad personal access token is answered 203 with the sign-in page, and
    // an expired one with a redirect.
    for status in [203, 302, 401, 403] {
        let org = Org::new(move |_| res(status, Value::Null));
        let changes = ado(&org).changed(&repos(), &none()).await.unwrap();
        let want = if status == 403 { 403 } else { 401 };
        assert_eq!(changes.denied["contoso/Platform"], want, "status {status}");
        assert!(changes.issues.is_empty() && changes.cursors.is_empty());
    }
}

#[tokio::test]
async fn a_failed_saved_query_stalls_the_project_and_keeps_its_cursor() {
    let org = Org::new(|req| {
        if path(req).contains("/wit/queries/") {
            return res(404, Value::Null);
        }
        if path(req).ends_with("/wiql") {
            return res(200, json!({ "workItems": [{ "id": 12 }] }));
        }
        res(200, json!({ "value": [raw(12, |_| {})] }))
    });
    let cfg = config(json!({ "ado_query": "Shared Queries/Gone" }));
    let changes = ado_with(&org, &cfg)
        .changed(&repos(), &none())
        .await
        .unwrap();
    assert_eq!(changes.denied["contoso/Platform"], 404);
    assert!(changes.issues.is_empty() && changes.cursors.is_empty());
}

#[tokio::test]
async fn a_rate_limit_waits_and_holds_every_call_until_it_passes() {
    let org = Org::new(|_| {
        let mut r = res(429, Value::Null);
        r.headers.insert("retry-after".into(), "7".into());
        r
    });
    let a = ado(&org);
    let Err(SourceError::RateLimited { wait }) = a.changed(&repos(), &none()).await else {
        panic!("expected a rate limit");
    };
    assert_eq!(wait, Duration::from_secs(7));
    assert!(a.changed(&repos(), &none()).await.is_err());
    assert_eq!(org.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_moved_item_is_followed_and_a_deleted_one_is_gone() {
    let org = Org::new(|req| {
        if path(req).ends_with("/workitems/12") {
            return res(
                200,
                raw(12, |v| {
                    v["fields"]["System.TeamProject"] = json!("Other Team")
                }),
            );
        }
        res(404, Value::Null)
    });
    let cfg = Config::parse(
        &json!({
            "tracker": { "url": "http://t", "token_env": "T" },
            "ado": { "organization": "contoso", "pat_env": "P",
                     "projects": { "Platform": "Platform Team", "Other": "Other Team" } },
            "rules": [
                { "name": "a", "repo": "contoso/Platform", "mode": "admit" },
                { "name": "b", "repo": "contoso/Other", "mode": "admit" }
            ],
        })
        .to_string(),
    )
    .unwrap();
    let a = ado_with(&org, &cfg);
    assert_eq!(
        a.fetch(&IssueRef::new("contoso/Platform", 12))
            .await
            .unwrap(),
        Fetched::Moved(IssueRef::new("contoso/Other", 12))
    );
    assert_eq!(
        a.fetch(&IssueRef::new("contoso/Platform", 99))
            .await
            .unwrap(),
        Fetched::Gone
    );
}

#[test]
fn priority_one_to_four_maps_to_tiers_and_a_rule_can_override() {
    let org = Org::new(|_| res(200, Value::Null));
    let a = ado(&org);
    let tier = |rule: &Rule, p: Option<u64>| {
        let issue = a
            .parse(&raw(1, |v| {
                v["fields"]["Microsoft.VSTS.Common.Priority"] = json!(p);
            }))
            .unwrap();
        rule.tier_for(&issue)
    };
    let plain = config(json!({})).rules.remove(0);
    let tiers: Vec<u8> = [1, 2, 3, 4]
        .iter()
        .map(|p| tier(&plain, Some(*p)))
        .collect();
    assert_eq!(tiers, [0, 1, 2, 3]);
    // No priority, or one outside 1 to 4, takes the rule's default.
    assert_eq!(tier(&plain, None), 2);
    assert_eq!(tier(&plain, Some(9)), 2);
    let custom = config(json!({ "priority_labels": { "2": 0 }, "default_tier": 3 }))
        .rules
        .remove(0);
    assert_eq!(tier(&custom, Some(2)), 0);
    assert_eq!(tier(&custom, None), 3);
}

#[test]
fn an_area_path_maps_by_its_longest_ancestor_and_not_by_a_shared_prefix() {
    let rule = config(json!({
        "area_paths": {
            "Platform Team\\Storage": "GetFelix/felix",
            "Platform Team\\Storage\\Docs": "GetFelix/felix-docs"
        }
    }))
    .rules
    .remove(0);
    let org = Org::new(|_| res(200, Value::Null));
    let a = ado(&org);
    let target = |area: &str| {
        let issue = a
            .parse(&raw(1, |v| v["fields"]["System.AreaPath"] = json!(area)))
            .unwrap();
        rule.target_for(&issue)
    };
    assert_eq!(
        target("Platform Team\\Storage").as_deref(),
        Some("GetFelix/felix")
    );
    assert_eq!(
        target("platform team\\storage\\Replication").as_deref(),
        Some("GetFelix/felix")
    );
    assert_eq!(
        target("Platform Team\\Storage\\Docs\\Site").as_deref(),
        Some("GetFelix/felix-docs")
    );
    assert_eq!(target("Platform Team\\Storage2"), None);
    assert_eq!(target("Platform Team"), None);
}

fn engine(org: &Org, tracker: &Fake, extra: Value) -> Ingest<Ado<Org>, Fake> {
    let cfg = config(extra);
    let repos = cfg.repos();
    Ingest::new(
        ado_with(org, &cfg),
        tracker.clone(),
        Settings {
            rules: cfg.rules.clone(),
            repos,
            reconcile_period: None,
            release_compares: 20,
        },
    )
}

#[tokio::test]
async fn an_item_in_an_unmapped_area_goes_to_needs_you_and_returns_when_mapped() {
    let org = listing(
        vec![raw(12, |v| {
            v["fields"]["System.AreaPath"] = json!("Platform Team\\Docs");
        })],
        vec![],
    );
    let tracker = Fake::default();
    engine(&org, &tracker, json!({})).poll(false).await.unwrap();
    let item = tracker.item_of("contoso/Platform#12");
    assert_eq!(tracker.state().needs_you(&item), Some(NeedsYou::LeftScope));
    tracker.person(kbl_core::work::WorkRecord::FormatRaised { to: 2 });
    let mapped = engine(&org, &tracker, json!({ "target_repo": "GetFelix/felix" }));
    mapped.poll(false).await.unwrap();
    assert_eq!(tracker.state().needs_you(&item), None);
}

#[tokio::test]
async fn re_polling_never_imports_an_item_twice_and_a_pull_request_is_announced_once() {
    let org = listing(vec![raw(12, |_| {})], vec![]);
    let tracker = Fake::default();
    let ingest = engine(&org, &tracker, json!({}));
    ingest.poll(false).await.unwrap();
    let item = tracker.item_of("contoso/Platform#12");
    tracker.agent(
        "w1",
        kbl_core::work::WorkRecord::Claimed {
            item: item.clone(),
            attempt: 1,
            deadline_ms: 90_000,
            direct: false,
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
    assert_eq!(tracker.count("imported"), 1);
    let paths: Vec<String> = org.writes().into_iter().map(|w| w.0).collect();
    assert_eq!(
        paths,
        [
            "PATCH /contoso/Platform%20Team/_apis/wit/workitems/12",
            "POST /contoso/Platform%20Team/_apis/wit/workItems/12/comments",
            "PATCH /contoso/Platform%20Team/_apis/wit/workitems/12",
        ]
    );
}

#[tokio::test]
async fn a_pull_request_outside_the_mapped_repositories_is_not_announced() {
    let org = listing(vec![raw(12, |_| {})], vec![]);
    let tracker = Fake::default();
    let ingest = engine(&org, &tracker, json!({}));
    ingest.poll(false).await.unwrap();
    let item = tracker.item_of("contoso/Platform#12");
    tracker.agent(
        "w1",
        kbl_core::work::WorkRecord::Claimed {
            item: item.clone(),
            attempt: 1,
            deadline_ms: 90_000,
            direct: false,
        },
    );
    tracker.agent(
        "w1",
        kbl_core::work::WorkRecord::Progress {
            item,
            attempt: 1,
            note: kbl_core::content::Content::Text("fixing".into()),
            branch: None,
            pr: Some("Evil/repo#1".into()),
        },
    );
    ingest.poll(false).await.unwrap();
    assert!(org.writes().is_empty());
}

#[test]
fn an_unknown_credential_with_no_own_logins_fails_startup() {
    let org = Org::new(|_| res(403, Value::Null));
    let mut a = ado(&org);
    a.own.clear();
    let err = a.require_own().unwrap_err().to_string();
    assert!(err.contains("own_logins"), "{err}");
    a.add_own(vec!["KBL".into()]);
    assert!(a.require_own().is_ok());
}

#[tokio::test]
async fn the_credential_names_itself_through_connection_data() {
    let org = Org::new(|req| {
        assert!(
            path(req).ends_with("/contoso/_apis/connectionData"),
            "{}",
            req.url
        );
        res(
            200,
            json!({ "authenticatedUser": { "providerDisplayName": "KBL Bot", "customDisplayName": "kbl" } }),
        )
    });
    assert_eq!(ado(&org).login().await, ["KBL Bot", "kbl"]);
}

#[test]
fn html_becomes_plain_text() {
    assert_eq!(
        html_text("<p>a &lt;b&gt;</p><ul><li>x</li><li>y &amp;amp;</li></ul>"),
        "a <b>\nx\ny &amp;"
    );
}

mod signing {
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use transport::{Auth, Entra, Reqwest, Secret};

    use super::*;

    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    fn secret_file(name: &str, text: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("kbl-ado-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("secret");
        std::fs::write(&file, text).unwrap();
        (dir, file)
    }

    fn form(body: &str) -> BTreeMap<String, String> {
        reqwest::Url::parse(&format!("http://h/?{body}"))
            .unwrap()
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn who() -> axum::routing::MethodRouter {
        get(|h: HeaderMap| async move {
            axum::Json(json!({ "auth": h["authorization"].to_str().unwrap() }))
        })
    }

    #[tokio::test]
    async fn a_personal_access_token_goes_out_as_basic_with_no_user_and_patch_is_json_patch() {
        let app = axum::Router::new().route("/who", who()).route(
            "/edit",
            axum::routing::patch(|h: HeaderMap| async move {
                axum::Json(json!({ "type": h["content-type"].to_str().unwrap() }))
            }),
        );
        let base = serve(app).await;
        let (dir, file) = secret_file("pat", "s3cret\n");
        let t = Reqwest::new(Auth::Pat(Secret::File(file))).unwrap();
        let got = t.send(Req::get(format!("{base}/who"))).await.unwrap();
        // base64 of ":s3cret"
        assert_eq!(got.body["auth"], "Basic OnMzY3JldA==");
        let patch = Req {
            method: "PATCH",
            url: format!("{base}/edit"),
            if_none_match: None,
            body: Some(json!([])),
        };
        let got = t.send(patch).await.unwrap();
        assert_eq!(got.body["type"], "application/json-patch+json");
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn an_unreadable_or_empty_credential_fails_without_naming_its_value() {
        let (dir, file) = secret_file("empty", " \n");
        let t = Reqwest::new(Auth::Pat(Secret::File(file))).unwrap();
        let err = format!(
            "{:#}",
            t.send(Req::get("http://127.0.0.1:1/".into()))
                .await
                .unwrap_err()
        );
        assert!(err.contains("is empty"), "{err}");
        let t = Reqwest::new(Auth::Pat(Secret::Env("KBL_ADO_TEST_UNSET_PAT".into()))).unwrap();
        let err = format!(
            "{:#}",
            t.send(Req::get("http://127.0.0.1:1/".into()))
                .await
                .unwrap_err()
        );
        assert!(err.contains("KBL_ADO_TEST_UNSET_PAT is not set"), "{err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn a_service_principal_signs_in_with_the_azure_devops_scope_and_caches_the_token() {
        let seen: Arc<Mutex<Vec<BTreeMap<String, String>>>> = Arc::default();
        let log = seen.clone();
        let app = axum::Router::new()
            .route(
                "/tenant-1/oauth2/v2.0/token",
                post(move |body: String| {
                    let log = log.clone();
                    async move {
                        log.lock().unwrap().push(form(&body));
                        axum::Json(json!({ "access_token": "entra-token", "expires_in": 3600 }))
                    }
                }),
            )
            .route("/who", who());
        let base = serve(app).await;
        let (dir, file) = secret_file("entra", "client-s3cret\n");
        let t = Reqwest::new(Auth::Entra(Entra::new(
            "tenant-1",
            "app-1",
            Secret::File(file),
            &base,
        )))
        .unwrap();
        for _ in 0..2 {
            let got = t.send(Req::get(format!("{base}/who"))).await.unwrap();
            assert_eq!(got.body["auth"], "Bearer entra-token");
        }
        let seen = seen.lock().unwrap();
        // Signed in once, and for the Azure DevOps resource only.
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["grant_type"], "client_credentials");
        assert_eq!(seen[0]["client_id"], "app-1");
        assert_eq!(seen[0]["client_secret"], "client-s3cret");
        assert_eq!(
            seen[0]["scope"],
            "499b84ac-1321-427f-aa17-267ca6975798/.default"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn a_rejected_service_principal_is_an_error_that_holds_no_secret() {
        let app = axum::Router::new().route(
            "/tenant-1/oauth2/v2.0/token",
            post(|body: String| async move {
                let f = form(&body);
                // The provider echoing the secret back must not reach an error.
                (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(json!({
                        "error": "invalid_client",
                        "error_description": format!("bad secret {}", f["client_secret"])
                    })),
                )
            }),
        );
        let base = serve(app).await;
        let (dir, file) = secret_file("bad", "client-s3cret\n");
        let t = Reqwest::new(Auth::Entra(Entra::new(
            "tenant-1",
            "app-1",
            Secret::File(file),
            &base,
        )))
        .unwrap();
        let err = format!(
            "{:#}",
            t.send(Req::get(format!("{base}/who"))).await.unwrap_err()
        );
        assert!(
            err.contains("401") && err.contains("invalid_client"),
            "{err}"
        );
        assert!(!err.contains("client-s3cret"), "{err}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[tokio::test]
    async fn a_revoked_token_is_replaced_once() {
        let n = Arc::new(Mutex::new(0u32));
        let c = n.clone();
        let app = axum::Router::new()
            .route(
                "/t/oauth2/v2.0/token",
                post(move || {
                    let c = c.clone();
                    async move {
                        let mut n = c.lock().unwrap();
                        *n += 1;
                        axum::Json(json!({ "access_token": format!("tok{n}"), "expires_in": 3600 }))
                    }
                }),
            )
            .route(
                "/who",
                get(|h: HeaderMap| async move {
                    if h["authorization"] == "Bearer tok1" {
                        return (StatusCode::UNAUTHORIZED, axum::Json(json!({})));
                    }
                    (
                        StatusCode::OK,
                        axum::Json(json!({ "auth": h["authorization"].to_str().unwrap() })),
                    )
                }),
            );
        let base = serve(app).await;
        let (dir, file) = secret_file("revoked", "s\n");
        let t = Reqwest::new(Auth::Entra(Entra::new("t", "a", Secret::File(file), &base))).unwrap();
        let got = t.send(Req::get(format!("{base}/who"))).await.unwrap();
        assert_eq!(got.body["auth"], "Bearer tok2");
        std::fs::remove_dir_all(dir).ok();
    }
}

#[tokio::test]
async fn a_write_to_an_item_that_left_its_project_is_refused() {
    let org = Org::new(|req| {
        if req.method == "GET" {
            return res(
                200,
                raw(12, |v| {
                    v["fields"]["System.TeamProject"] = json!("Other Team")
                }),
            );
        }
        res(200, Value::Null)
    });
    let a = ado(&org);
    let issue = IssueRef::new("contoso/Platform", 12);
    for what in [
        WriteBack::LinkPullRequest {
            issue: issue.clone(),
            pr: IssueRef::new("GetFelix/felix", 40),
        },
        WriteBack::Transition {
            issue,
            name: "Resolved".into(),
        },
    ] {
        assert!(a.write_back(&what).await.is_err());
    }
    assert!(org.writes().is_empty());
}

#[tokio::test]
async fn a_dependency_in_an_unserved_project_leaves_the_listing_incomplete() {
    let org = Org::new(|req| {
        if path(req).ends_with("/workitems/12") {
            return res(
                200,
                raw(12, |v| {
                    v["relations"] = json!([{
                        "rel": "System.LinkTypes.Dependency-Reverse",
                        "url": "https://dev.azure.com/contoso/_apis/wit/workItems/77"
                    }]);
                }),
            );
        }
        res(
            200,
            json!({ "value": [{ "id": 77, "fields": { "System.TeamProject": "Elsewhere" } }] }),
        )
    });
    let links = ado(&org)
        .links(&IssueRef::new("contoso/Platform", 12))
        .await
        .unwrap();
    assert!(links.blocked_by.is_empty() && !links.complete);
}

#[tokio::test]
async fn a_redirected_service_principal_request_signs_in_again() {
    use axum::routing::{get, post};
    let n = Arc::new(Mutex::new(0u32));
    let c = n.clone();
    let app = axum::Router::new()
        .route(
            "/t/oauth2/v2.0/token",
            post(move || {
                let c = c.clone();
                async move {
                    let mut n = c.lock().unwrap();
                    *n += 1;
                    axum::Json(json!({ "access_token": format!("tok{n}"), "expires_in": 3600 }))
                }
            }),
        )
        .route(
            "/who",
            get(|h: axum::http::HeaderMap| async move {
                if h["authorization"] == "Bearer tok1" {
                    return (
                        axum::http::StatusCode::FOUND,
                        [("location", "https://login.example/")],
                        String::new(),
                    );
                }
                (
                    axum::http::StatusCode::OK,
                    [("location", "")],
                    "{}".to_owned(),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = std::env::temp_dir().join(format!("kbl-ado-redir-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("secret");
    std::fs::write(&file, "s").unwrap();
    let t = transport::Reqwest::new(transport::Auth::Entra(transport::Entra::new(
        "t",
        "a",
        transport::Secret::File(file),
        &base,
    )))
    .unwrap();
    let got = t.send(Req::get(format!("{base}/who"))).await.unwrap();
    assert_eq!(got.status, 200);
    assert_eq!(*n.lock().unwrap(), 2);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn a_transient_400_on_a_state_change_is_retried_not_dropped() {
    let org = Org::new(|req| {
        if req.method == "GET" {
            return res(200, raw(12, |_| {}));
        }
        res(400, json!({ "message": "The request timed out upstream" }))
    });
    let result = ado(&org)
        .write_back(&WriteBack::Transition {
            issue: IssueRef::new("contoso/Platform", 12),
            name: "Closed".into(),
        })
        .await;
    assert!(result.is_err());
}

/// Eight items in one change instant, a cap of five: the second poll must
/// resume after the last id, not read the first five again.
#[tokio::test]
async fn a_tie_wider_than_the_cap_pages_by_id() {
    let items: Vec<Value> = (1..=8).map(|i| raw(i, |_| {})).collect();
    let org = Org::new(move |req| {
        let p = path(req);
        if p.ends_with("/_apis/wit/wiql") {
            let q = req.body.as_ref().unwrap()["query"].as_str().unwrap();
            let after = q
                .split("[System.Id] > ")
                .nth(1)
                .and_then(|r| r.trim_end_matches(')').split(')').next())
                .and_then(|n| n.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let ids: Vec<Value> = items
                .iter()
                .filter(|i| i["id"].as_u64().unwrap() > after)
                .take(LIST_CAP)
                .map(|i| json!({ "id": i["id"] }))
                .collect();
            return res(200, json!({ "workItems": ids }));
        }
        if p.ends_with("/_apis/wit/workitems") {
            let want: Vec<u64> = param(req, "ids")
                .unwrap()
                .split(',')
                .filter_map(|n| n.parse().ok())
                .collect();
            let found: Vec<Value> = items
                .iter()
                .filter(|i| want.contains(&i["id"].as_u64().unwrap()))
                .cloned()
                .collect();
            return res(200, json!({ "value": found }));
        }
        if p.ends_with("/comments") {
            return res(200, json!({ "comments": [] }));
        }
        res(200, Value::Null)
    });
    let a = ado(&org);
    let first = a.changed(&repos(), &none()).await.unwrap();
    let cursor = first.cursors["contoso/Platform"].clone();
    assert_eq!(first.issues.len(), 5);
    assert_eq!(cursor.issues_after_id, Some(5));
    let cursors = BTreeMap::from([("contoso/Platform".to_owned(), cursor)]);
    let second = a.changed(&repos(), &cursors).await.unwrap();
    let mut ids: Vec<u64> = second.issues.iter().map(|i| i.id.number).collect();
    ids.sort();
    assert_eq!(ids, vec![6, 7, 8]);
    assert_eq!(second.cursors["contoso/Platform"].issues_after_id, None);
}
