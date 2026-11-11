use agents_core::work::WorkRecord;
use agents_core::work_fold::{Closure, NeedsYou, Stage};
use serde_json::{Value, json};

use super::*;
use crate::config::Config;
use crate::github::Github;
use crate::testing::{Fake, Script, fixture, issue, labels, ok, status};

const LIST: &str = "/repos/GetFelix/felix/issues?";

struct Rig {
    gh: Script,
    tracker: Fake,
}

fn config(rules: Value) -> Config {
    Config::parse(
        &json!({
            "tracker": { "url": "http://t", "token_env": "T" },
            "github": { "token_env": "G" },
            "reconcile_every": 1000,
            "rules": rules,
        })
        .to_string(),
    )
    .unwrap()
}

fn admit_rule() -> Value {
    json!({
        "name": "agent-ok", "repo": "GetFelix/felix", "labels_all": ["agent-ok"],
        "labels_excluded": ["wontfix"], "mode": "admit",
        "priority_labels": { "p0": 0, "p1": 1 }
    })
}

fn rig() -> Rig {
    let gh = Script::default();
    gh.on("GET", "/issues/comments?", ok(json!([])));
    Rig {
        gh,
        tracker: Fake::default(),
    }
}

impl Rig {
    fn ingest(&self, rules: Value) -> Ingest<Github<Script>, Fake> {
        let cfg = config(rules);
        let repos = cfg.repos();
        Ingest::new(
            Github::new(
                "https://api.github.com",
                self.gh.clone(),
                vec!["Kibble[bot]".into()],
            ),
            self.tracker.clone(),
            Settings {
                rules: cfg.rules,
                repos,
                reconcile_every: cfg.reconcile_every,
            },
        )
    }

    fn list(&self, issues: Vec<Value>) {
        self.gh.on("GET", LIST, ok(Value::Array(issues)));
    }

    fn comments(&self, comments: Vec<Value>) {
        self.gh
            .on("GET", "/issues/comments?", ok(Value::Array(comments)));
    }

    fn root(&self, up: &str) -> agents_core::names::ItemId {
        self.tracker.item_of(up)
    }

    /// An agent claims the root and reports a pull request.
    fn hold(&self, up: &str, pr: Option<&str>) -> agents_core::names::ItemId {
        let item = self.root(up);
        self.tracker.agent(
            "w1",
            WorkRecord::Claimed {
                item: item.clone(),
                attempt: 1,
                deadline_ms: 90_000,
            },
        );
        if let Some(pr) = pr {
            self.tracker.agent(
                "w1",
                WorkRecord::Progress {
                    item: item.clone(),
                    attempt: 1,
                    note: agents_core::content::Content::Text("fixing".into()),
                    branch: Some("fix/stale-read".into()),
                    pr: Some(pr.into()),
                },
            );
        }
        item
    }
}

fn at(mut v: Value, updated: &str) -> Value {
    v["updated_at"] = json!(updated);
    v
}

fn closed(mut v: Value, reason: &str, updated: &str) -> Value {
    v["state"] = json!("closed");
    v["state_reason"] = json!(reason);
    at(v, updated)
}

const UP: &str = "GetFelix/felix#812";

#[tokio::test]
async fn an_issue_is_imported_once_across_re_polls_and_a_lost_cursor() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    assert_eq!(
        rig.tracker.kinds(),
        [
            "imported", "context", "context", "accepted", "proposed", "proposed"
        ]
    );
    let s = rig.tracker.state();
    let root = &s.items[&rig.root(UP)];
    // The p1 label sets the tier, and an admitted root gets its two items.
    assert_eq!(root.tier, Some(1));
    assert_eq!(root.children.len(), 2);
    assert_eq!(s.stage(&root.id), Some(Stage::Ready));

    // The same listing again, and a redelivered webhook's extra poll.
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("imported"), 1);
    assert_eq!(rig.tracker.kinds().len(), 6);

    // A restart that lost its saved state finds the root and adds nothing.
    rig.tracker.forget_saved();
    rig.ingest(json!([admit_rule()])).poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("imported"), 1);
    assert_eq!(rig.tracker.count("accepted"), 1);
    assert_eq!(rig.tracker.state().items.len(), 3);
}

fn imported_record() -> WorkRecord {
    WorkRecord::Imported {
        item: item_id(UP),
        source: "github".into(),
        upstream_id: UP.into(),
        title: "stale read".into(),
    }
}

async fn finishes_the_import(first: Vec<WorkRecord>) {
    let rig = rig();
    for r in first {
        rig.tracker.append(&r).await.unwrap();
    }
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    let root = &s.items[&item_id(UP)];
    let entries: Vec<_> = root.context.keys().map(|k| k.as_str().to_owned()).collect();
    assert_eq!(entries, ["intake", "issue"]);
    assert_eq!(root.tier, Some(1));
    assert_eq!(root.children.len(), 2);
    assert_eq!(s.stage(&root.id), Some(Stage::Ready));
    assert_eq!(rig.tracker.count("imported"), 1);
}

#[tokio::test]
async fn a_crash_right_after_imported_is_finished_by_the_next_poll() {
    finishes_the_import(vec![imported_record()]).await;
}

#[tokio::test]
async fn a_crash_right_after_accepted_is_finished_by_the_next_poll() {
    finishes_the_import(vec![
        imported_record(),
        WorkRecord::Accepted {
            item: item_id(UP),
            tier: Some(1),
        },
    ])
    .await;
}

#[tokio::test]
async fn a_pull_request_in_another_repo_is_never_written_to() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    rig.hold(UP, Some("Other/repo#2210"));
    rig.gh.on("GET", "/pulls/2210", ok(fixture("pull")));
    rig.gh.on("POST", "/graphql", ok(json!({ "data": {} })));
    rig.gh.on("POST", "/comments", status(201, &[]));
    rig.list(vec![closed(
        issue(812, |_| {}),
        "not_planned",
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    assert!(rig.gh.sent().iter().all(|r| !r.url.contains("Other/repo")));
    assert!(rig.gh.writes().is_empty());
}

#[tokio::test]
async fn a_root_a_person_dismissed_is_not_reopened_with_the_issue() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    let item = rig.root(UP);
    rig.tracker
        .append(&WorkRecord::Closed {
            item: item.clone(),
            why: CloseWhy::Dismissed,
        })
        .await
        .unwrap();
    rig.list(vec![closed(
        issue(812, |_| {}),
        "completed",
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    let mut open = issue(812, |_| {});
    open["state_reason"] = json!("reopened");
    rig.list(vec![at(open, "2026-11-10T13:00:00Z")]);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("reopened"), 0);
    assert_eq!(
        rig.tracker.state().items[&item].closed,
        Some(Closure::Dismissed)
    );
}

#[test]
fn only_a_plain_owner_repo_number_is_a_reference() {
    assert!(IssueRef::parse("GetFelix/felix#812").is_some());
    for bad in [
        "a/b/c#1", "a/b#+1", "a/b#01", "a/b#0x1", "/b#1", "a/#1", "a/../b#1", "a b/c#1", "a/b#1#2",
        "a/b",
    ] {
        assert!(IssueRef::parse(bad).is_none(), "{bad}");
    }
}

#[tokio::test]
async fn an_issue_that_matches_no_rule_or_an_excluded_label_is_not_imported() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![
        issue(1, |v| labels(v, &["bug"])),
        issue(2, |v| labels(v, &["agent-ok", "wontfix"])),
        closed(issue(3, |_| {}), "completed", "2026-11-10T12:00:00Z"),
    ]);
    ingest.poll(false).await.unwrap();
    assert!(rig.tracker.kinds().is_empty());
}

#[tokio::test]
async fn a_label_removed_while_an_agent_holds_the_item_stops_it_after_the_attempt() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    let item = rig.hold(UP, None);

    rig.list(vec![at(
        issue(812, |v| labels(v, &["p1"])),
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("left_scope"), 1);
    // The holder keeps its attempt.
    assert_eq!(rig.tracker.state().stage(&item), Some(Stage::Held));

    rig.tracker.agent(
        "w1",
        WorkRecord::Released {
            item: item.clone(),
            attempt: 1,
            reason: None,
            cost_micros: 0,
        },
    );
    let s = rig.tracker.state();
    assert_eq!(s.needs_you(&item), Some(NeedsYou::LeftScope));
    assert_eq!(s.stage(&item), Some(Stage::NeedsYou));
}

#[tokio::test]
async fn a_label_removed_from_a_free_item_withdraws_it_from_ready() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    rig.list(vec![at(
        issue(812, |v| labels(v, &["p1"])),
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    assert_eq!(
        rig.tracker.state().stage(&rig.root(UP)),
        Some(Stage::NeedsYou)
    );
}

#[tokio::test]
async fn closed_as_not_planned_drops_the_item_and_drafts_the_agents_pull_request() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    let item = rig.hold(UP, Some("GetFelix/felix#2210"));
    rig.gh.on("GET", "/pulls/2210", ok(fixture("pull")));
    rig.gh.on("POST", "/graphql", ok(json!({ "data": {} })));
    rig.gh.on("POST", "/issues/2210/comments", status(201, &[]));

    rig.list(vec![closed(
        issue(812, |_| {}),
        "not_planned",
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();

    let s = rig.tracker.state();
    assert_eq!(s.items[&item].closed, Some(Closure::Upstream));
    assert!(s.items[&item].hold.is_none());
    for c in &s.items[&item].children {
        assert_eq!(s.items[c].closed, Some(Closure::Cancelled));
    }
    // The pull request becomes a draft and gets a comment; nothing else is written upstream.
    assert_eq!(
        rig.gh.writes(),
        [
            "POST https://api.github.com/graphql",
            "POST https://api.github.com/repos/GetFelix/felix/issues/2210/comments"
        ]
    );
    let draft = rig
        .gh
        .sent()
        .into_iter()
        .find(|r| r.url.ends_with("/graphql"))
        .unwrap();
    assert_eq!(draft.body.unwrap()["variables"]["id"], "PR_kwDOAAAA2210");

    // Polled again, it is not closed or written to twice.
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("closed"), 1);
    assert_eq!(rig.gh.writes().len(), 2);
}

#[tokio::test]
async fn a_dropped_issue_with_an_already_draft_pull_request_only_gets_the_comment() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    rig.hold(UP, Some("GetFelix/felix#2210"));
    let mut pull = fixture("pull");
    pull["draft"] = json!(true);
    rig.gh.on("GET", "/pulls/2210", ok(pull));
    rig.gh.on("POST", "/issues/2210/comments", status(201, &[]));
    rig.list(vec![closed(
        issue(812, |_| {}),
        "not_planned",
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.gh.writes().len(), 1);
}

#[tokio::test]
async fn closed_as_completed_finishes_the_item_and_a_reopen_returns_it_to_triage() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    let item = rig.root(UP);
    let budget = rig.tracker.state().budgets[&item].clone();

    rig.list(vec![closed(
        issue(812, |_| {}),
        "completed",
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    assert_eq!(s.items[&item].closed, Some(Closure::Completed));
    assert_eq!(s.stage(&item), Some(Stage::Done));
    // Completed is not a drop: no write upstream.
    assert!(rig.gh.writes().is_empty());

    let mut open = issue(812, |_| {});
    open["state_reason"] = json!("reopened");
    rig.list(vec![at(open, "2026-11-10T13:00:00Z")]);
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    assert_eq!(rig.tracker.count("reopened"), 1);
    assert_eq!(s.stage(&item), Some(Stage::Triage));
    assert_eq!(s.budgets[&item], budget);
    assert_eq!(rig.tracker.count("imported"), 1);
}

#[tokio::test]
async fn an_edit_flags_the_item_and_makes_no_work() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    let item = rig.root(UP);
    let items_before = rig.tracker.state().items.len();

    // Only the update time moved: nothing changed that matters.
    rig.list(vec![at(issue(812, |_| {}), "2026-11-10T11:00:00Z")]);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("updated"), 0);

    rig.list(vec![at(
        issue(812, |v| {
            v["title"] = json!("Reads miss a fresh write on QUIC")
        }),
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("updated"), 1);
    let s = rig.tracker.state();
    assert!(s.items[&item].changed_upstream);
    assert_eq!(s.items[&item].title, "Reads miss a fresh write on QUIC");
    assert_eq!(s.items.len(), items_before);
}

#[tokio::test]
async fn a_comment_becomes_context_once_and_our_own_comments_are_not_imported() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    let before = rig.tracker.kinds().len();

    let bob = fixture("comment");
    let mut own = fixture("comment");
    own["id"] = json!(9002);
    own["user"]["login"] = json!("kibble[bot]");
    own["body"] = json!("The pull request is up.");
    rig.comments(vec![bob.clone(), own]);
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.kinds().len(), before + 1);
    let log = rig.tracker.state();
    assert!(
        log.items[&rig.root(UP)]
            .context
            .keys()
            .any(|k| k.as_str() == "gh-c9001")
    );
    assert!(
        !log.items[&rig.root(UP)]
            .context
            .keys()
            .any(|k| k.as_str() == "gh-c9002")
    );

    // An edited comment replaces its entry.
    let mut edited = bob;
    edited["updated_at"] = json!("2026-11-10T12:30:00Z");
    edited["body"] = json!("Only on the QUIC path, on 0.7.");
    rig.comments(vec![edited]);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.kinds().len(), before + 2);
}

#[tokio::test]
async fn a_propose_rule_stops_at_its_cap_and_picks_up_the_rest_as_roots_close() {
    let rig = rig();
    let rule = json!({
        "name": "milestone", "repo": "GetFelix/felix", "milestone": "0.8",
        "mode": "propose", "cap": 1
    });
    let ingest = rig.ingest(json!([rule]));
    let m = |n: u64, t: &str| {
        at(
            issue(n, |v| {
                v["milestone"] = json!({ "title": "0.8" });
                labels(v, &[]);
            }),
            t,
        )
    };
    rig.list(vec![
        m(21, "2026-11-10T10:00:00Z"),
        m(22, "2026-11-10T10:01:00Z"),
        m(23, "2026-11-10T10:02:00Z"),
    ]);
    rig.gh
        .on("GET", "/issues/22", ok(m(22, "2026-11-10T10:01:00Z")));
    rig.gh
        .on("GET", "/issues/23", ok(m(23, "2026-11-10T10:02:00Z")));
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();

    // One root, in Triage until a person accepts it; the others wait upstream.
    let s = rig.tracker.state();
    assert_eq!(s.imported().count(), 1);
    let first = rig.root("GetFelix/felix#21");
    assert_eq!(s.stage(&first), Some(Stage::Triage));
    assert_eq!(rig.tracker.count("accepted"), 0);
    assert_eq!(rig.tracker.count("proposed"), 0);

    // It closes upstream; the oldest waiting match takes its place.
    rig.list(vec![closed(
        m(21, "2026-11-10T10:00:00Z"),
        "completed",
        "2026-11-10T11:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    assert_eq!(s.imported().count(), 2);
    assert!(s.imported().any(|(_, u, _)| u == "GetFelix/felix#22"));
    assert!(!s.imported().any(|(_, u, _)| u == "GetFelix/felix#23"));
}

#[tokio::test]
async fn the_first_matching_rule_takes_the_issue() {
    let rig = rig();
    let propose = json!({ "name": "all", "repo": "GetFelix/felix", "mode": "propose" });
    let ingest = rig.ingest(json!([admit_rule(), propose]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    // The admit rule is first, so it is admitted, not proposed.
    assert_eq!(rig.tracker.count("accepted"), 1);
}

#[tokio::test]
async fn a_transferred_issue_keeps_its_item_under_its_new_id() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    let item = rig.root(UP);

    rig.gh.on(
        "GET",
        "/repos/GetFelix/felix/issues/812",
        status(
            301,
            &[(
                "location",
                "https://api.github.com/repos/GetFelix/felix2/issues/5",
            )],
        ),
    );
    let mut moved = issue(5, |_| {});
    moved["html_url"] = json!("https://github.com/GetFelix/felix2/issues/5");
    rig.gh
        .on("GET", "/repos/GetFelix/felix2/issues/5", ok(moved.clone()));
    ingest.poll(true).await.unwrap();
    assert_eq!(rig.tracker.count("imported"), 1);
    let ctx = rig.tracker.state().items[&item].context.clone();
    assert!(ctx.keys().any(|k| k.as_str() == "moved"));

    // The new id now lists as a change; it is the same item, not a second import.
    rig.list(vec![]);
    rig.gh
        .on("GET", "/repos/GetFelix/felix/issues?", ok(json!([])));
    rig.gh.on(
        "GET",
        "/repos/GetFelix/felix2/issues?",
        ok(json!([at(moved, "2026-11-10T12:00:00Z")])),
    );
    let cfg = json!([admit_rule(), {
        "name": "second", "repo": "GetFelix/felix2", "labels_all": ["agent-ok"], "mode": "admit"
    }]);
    rig.ingest(cfg).poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("imported"), 1);
}

#[tokio::test]
async fn an_issue_deleted_upstream_is_dropped_at_the_next_reconcile() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    rig.gh
        .on("GET", "/repos/GetFelix/felix/issues/812", status(404, &[]));
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("closed"), 0);
    ingest.poll(true).await.unwrap();
    assert_eq!(
        rig.tracker.state().items[&rig.root(UP)].closed,
        Some(Closure::Upstream)
    );
}

#[tokio::test]
async fn blocked_by_becomes_an_edge_once_both_issues_are_imported() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![
        at(issue(812, |_| {}), "2026-11-10T10:00:00Z"),
        at(issue(813, |_| {}), "2026-11-10T10:01:00Z"),
    ]);
    // 813 waits on 812, and on 900 which is not imported.
    rig.gh.on(
        "GET",
        "/issues/813/dependencies/blocked_by",
        ok(json!([
            { "number": 812, "repository_url": "https://api.github.com/repos/GetFelix/felix" },
            { "number": 900, "repository_url": "https://api.github.com/repos/GetFelix/felix" }
        ])),
    );
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    let (a, b) = (rig.root(UP), rig.root("GetFelix/felix#813"));
    assert_eq!(s.edges.iter().collect::<Vec<_>>(), [&(b.clone(), a)]);
    assert_eq!(s.stage(&b), Some(Stage::Blocked));
    assert_eq!(rig.tracker.count("linked"), 1, "{:?}", rig.tracker.kinds());
}

#[test]
fn the_item_id_follows_the_upstream_id() {
    assert_eq!(item_id(UP), item_id(UP));
    assert_ne!(item_id(UP), item_id("GetFelix/felix#813"));
    assert!(item_id(UP).as_str().len() <= 17);
}
