use kbl_core::work::{PrChecks, PrReview, WorkRecord, pr_branch};
use kbl_core::work_fold::{Closure, NeedsYou, PrWaits, Stage};
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
                vec!["KBL[bot]".into()],
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

    fn root(&self, up: &str) -> kbl_core::names::ItemId {
        self.tracker.item_of(up)
    }

    /// An agent claims the root and reports a pull request.
    fn hold(&self, up: &str, pr: Option<&str>) -> kbl_core::names::ItemId {
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
                    note: kbl_core::content::Content::Text("fixing".into()),
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
    rig.tracker.person(WorkRecord::Closed {
        item: item.clone(),
        why: CloseWhy::Dismissed,
    });
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
async fn an_issue_closed_with_no_reason_is_completed() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    let mut old = closed(issue(812, |_| {}), "completed", "2026-11-10T12:00:00Z");
    old["state_reason"] = Value::Null;
    rig.list(vec![old]);
    ingest.poll(false).await.unwrap();
    assert_eq!(
        rig.tracker.state().items[&rig.root(UP)].closed,
        Some(Closure::Completed)
    );
}

#[tokio::test]
async fn a_close_whose_save_failed_is_still_ours_when_the_issue_reopens() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    let item = rig.root(UP);

    rig.list(vec![closed(
        issue(812, |_| {}),
        "not_planned",
        "2026-11-10T12:00:00Z",
    )]);
    rig.tracker.fail_saves();
    ingest.poll(false).await.unwrap_err();
    assert_eq!(rig.tracker.count("closed"), 1);
    rig.tracker.allow_saves();
    ingest.poll(false).await.unwrap();

    let mut open = issue(812, |_| {});
    open["state_reason"] = json!("reopened");
    rig.list(vec![at(open, "2026-11-10T13:00:00Z")]);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("reopened"), 1);
    assert_eq!(rig.tracker.state().stage(&item), Some(Stage::Triage));
}

#[tokio::test]
async fn closed_issues_are_pruned_from_the_saved_state_and_still_reopen() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    let item = rig.root(UP);

    rig.list(vec![closed(
        issue(812, |_| {}),
        "completed",
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    assert!(rig.tracker.saved_state().items.is_empty());
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("closed"), 1);

    let mut open = issue(812, |_| {});
    open["state_reason"] = json!("reopened");
    rig.list(vec![at(open, "2026-11-10T13:00:00Z")]);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("reopened"), 1);
    assert_eq!(rig.tracker.count("imported"), 1);
    assert_eq!(rig.tracker.state().stage(&item), Some(Stage::Triage));
    assert!(rig.tracker.saved_state().items.contains_key(UP));
}

#[tokio::test]
async fn a_reopen_whose_save_failed_does_not_accept_the_root_again() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    rig.list(vec![closed(
        issue(812, |_| {}),
        "completed",
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    assert!(rig.tracker.saved_state().items.is_empty());

    let mut open = issue(812, |_| {});
    open["state_reason"] = json!("reopened");
    rig.list(vec![at(open, "2026-11-10T13:00:00Z")]);
    rig.tracker.fail_saves();
    ingest.poll(false).await.unwrap_err();
    rig.tracker.allow_saves();
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("accepted"), 1);
    assert_eq!(
        rig.tracker.state().stage(&rig.root(UP)),
        Some(Stage::Triage)
    );
}

#[tokio::test]
async fn a_root_ingest_reopened_closes_again_when_the_save_was_lost() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    rig.list(vec![closed(
        issue(812, |_| {}),
        "completed",
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    assert!(rig.tracker.saved_state().items.is_empty());

    // Ingest reopens the pruned root, and the save after it fails.
    let mut open = issue(812, |_| {});
    open["state_reason"] = json!("reopened");
    rig.list(vec![at(open, "2026-11-10T13:00:00Z")]);
    rig.tracker.fail_saves();
    ingest.poll(false).await.unwrap_err();
    assert_eq!(rig.tracker.count("reopened"), 1);
    // The issue closes again before a save lands.
    rig.tracker.allow_saves();
    rig.list(vec![closed(
        issue(812, |_| {}),
        "completed",
        "2026-11-10T14:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("closed"), 2);
    assert_eq!(
        rig.tracker.state().items[&rig.root(UP)].closed,
        Some(Closure::Completed)
    );
}

#[tokio::test]
async fn a_childless_root_ingest_reopened_is_not_accepted_on_a_retry() {
    let rig = rig();
    let mut propose = admit_rule();
    propose["mode"] = json!("propose");
    let ingest = rig.ingest(json!([propose]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    let item = rig.root(UP);
    rig.tracker.person(WorkRecord::Accepted {
        item: item.clone(),
        tier: Some(3),
    });
    rig.list(vec![closed(
        issue(812, |_| {}),
        "completed",
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    assert!(rig.tracker.saved_state().items.is_empty());

    // The rule now admits, and the poll that reopens the root fails to save.
    let ingest = rig.ingest(json!([admit_rule()]));
    let mut open = issue(812, |_| {});
    open["state_reason"] = json!("reopened");
    rig.list(vec![at(open, "2026-11-10T13:00:00Z")]);
    rig.tracker.fail_saves();
    ingest.poll(false).await.unwrap_err();
    rig.tracker.allow_saves();
    ingest.poll(false).await.unwrap();
    // Only the person's accept: the reopened root waits in Triage for a person.
    assert_eq!(rig.tracker.count("accepted"), 1);
    assert_eq!(rig.tracker.state().stage(&item), Some(Stage::Triage));
}

#[tokio::test]
async fn a_person_reopening_a_pruned_root_puts_it_back_in_the_saved_state() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    rig.list(vec![closed(
        issue(812, |_| {}),
        "completed",
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    rig.tracker
        .person(WorkRecord::Reopened { item: rig.root(UP) });
    ingest.poll(false).await.unwrap();
    assert!(rig.tracker.saved_state().items.contains_key(UP));
    assert_eq!(rig.tracker.count("closed"), 1);
}

#[tokio::test]
async fn a_dropped_issue_is_kept_until_its_pull_request_is_dealt_with() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    rig.hold(UP, Some("GetFelix/felix#2210"));
    rig.gh.on("GET", "/pulls/2210", status(500, &[]));
    rig.list(vec![closed(
        issue(812, |_| {}),
        "not_planned",
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    assert!(rig.tracker.saved_state().items.contains_key(UP));
}

#[tokio::test]
async fn comments_made_while_an_issue_waited_are_imported_when_it_is_admitted() {
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
    ]);
    rig.gh
        .on("GET", "/issues/22", ok(m(22, "2026-11-10T10:01:00Z")));
    let mut waiting = fixture("comment");
    waiting["id"] = json!(9100);
    waiting["issue_url"] = json!("https://api.github.com/repos/GetFelix/felix/issues/22");
    rig.gh
        .on("GET", "/issues/22/comments", ok(json!([waiting])));
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.state().imported().count(), 1);

    rig.list(vec![closed(
        m(21, "2026-11-10T10:00:00Z"),
        "completed",
        "2026-11-10T11:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    let item = rig.root("GetFelix/felix#22");
    let s = rig.tracker.state();
    assert!(
        s.items[&item]
            .context
            .keys()
            .any(|k| k.as_str() == "gh-c9100")
    );
    assert!(rig.tracker.saved_state().waiting.is_empty());
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
    own["user"]["login"] = json!("kbl[bot]");
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
async fn a_transferred_issue_that_closes_is_not_imported_again_when_it_reopens() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
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
    let shut = closed(moved.clone(), "completed", "2026-11-10T12:00:00Z");
    rig.gh
        .on("GET", "/repos/GetFelix/felix2/issues/5", ok(shut));
    let cfg = json!([admit_rule(), {
        "name": "second", "repo": "GetFelix/felix2", "labels_all": ["agent-ok"], "mode": "admit"
    }]);
    let ingest = rig.ingest(cfg);
    ingest.poll(true).await.unwrap();

    rig.gh
        .on("GET", "/repos/GetFelix/felix/issues?", ok(json!([])));
    rig.gh.on(
        "GET",
        "/repos/GetFelix/felix2/issues?",
        ok(json!([at(moved, "2026-11-10T13:00:00Z")])),
    );
    ingest.poll(false).await.unwrap();
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

#[tokio::test]
async fn a_refused_write_back_does_not_hold_the_cursor_and_is_given_up_on() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    rig.hold(UP, Some("GetFelix/felix#2210"));
    rig.gh.on("GET", "/pulls/2210", ok(fixture("pull")));
    rig.gh.on("POST", "/graphql", ok(json!({ "data": {} })));
    rig.gh.on("POST", "/issues/2210/comments", status(403, &[]));
    rig.list(vec![closed(
        issue(812, |_| {}),
        "not_planned",
        "2026-11-10T12:00:00Z",
    )]);
    ingest.poll(false).await.unwrap();
    let saved = rig.tracker.saved_state();
    assert_eq!(
        saved.cursors["GetFelix/felix"].issues_since.as_deref(),
        Some("2026-11-10T12:00:00Z")
    );
    assert_eq!(saved.items[UP].write_back_tries, 1);
    assert!(!saved.items[UP].written_back);
    ingest.poll(false).await.unwrap();
    ingest.poll(false).await.unwrap();
    let saved = rig.tracker.saved_state();
    // Given up on, so it is settled and forgotten.
    assert!(!saved.items.contains_key(UP));
    let comments = |gh: &Script| {
        gh.writes()
            .iter()
            .filter(|w| w.contains("/comments"))
            .count()
    };
    assert_eq!(comments(&rig.gh), 3);
    ingest.poll(false).await.unwrap();
    assert_eq!(comments(&rig.gh), 3);
}

#[tokio::test]
async fn when_the_poll_and_the_save_both_fail_the_poll_error_is_returned() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.gh.on("GET", LIST, status(500, &[]));
    rig.tracker.fail_saves();
    let err = ingest.poll(false).await.unwrap_err();
    assert!(err.to_string().contains("500"), "{err}");
}

// ---- one cursor per repo ----

fn two_repo_rules() -> Value {
    json!([admit_rule(), {
        "name": "other", "repo": "GetFelix/other", "labels_all": ["agent-ok"], "mode": "admit"
    }])
}

#[tokio::test]
async fn a_repo_answering_404_stalls_only_itself_and_is_reported_then_cleared() {
    let rig = rig();
    let ingest = rig.ingest(two_repo_rules());
    rig.gh
        .on("GET", "/repos/GetFelix/other/issues?", status(404, &[]));
    rig.list(vec![at(issue(812, |_| {}), "2026-11-10T08:00:00Z")]);
    ingest.poll(false).await.unwrap();

    let saved = rig.tracker.saved_state();
    // The readable repo advanced; the broken one has no cursor to pin anything.
    assert_eq!(
        saved.cursors["GetFelix/felix"].issues_since.as_deref(),
        Some("2026-11-10T08:00:00Z")
    );
    assert!(!saved.cursors.contains_key("GetFelix/other"));
    assert_eq!(rig.tracker.count("source_stalled"), 1);
    let s = rig.tracker.state();
    assert_eq!(
        s.stalled_sources().collect::<Vec<_>>(),
        [("github", "GetFelix/other", 404)]
    );

    // Still broken: not reported again, and the other repo keeps moving.
    rig.list(vec![at(issue(813, |_| {}), "2026-11-10T09:00:00Z")]);
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("source_stalled"), 1);
    assert_eq!(
        rig.tracker.saved_state().cursors["GetFelix/felix"]
            .issues_since
            .as_deref(),
        Some("2026-11-10T09:00:00Z")
    );
    assert_eq!(rig.tracker.state().imported().count(), 2);

    // Access returns: its issues arrive and the stall clears.
    rig.gh.on(
        "GET",
        "/repos/GetFelix/other/issues?",
        ok(json!([at(issue(7, |_| {}), "2026-11-10T10:00:00Z")])),
    );
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("source_stalled"), 2);
    assert_eq!(rig.tracker.state().stalled_sources().count(), 0);
    assert!(
        rig.tracker
            .state()
            .imported()
            .any(|(_, up, _)| up == "GetFelix/other#7")
    );
}

#[tokio::test]
async fn the_old_shared_cursor_seeds_every_repo_once() {
    let rig = rig();
    let ingest = rig.ingest(two_repo_rules());
    let old = json!({
        "cursor": {
            "issues_since": "2026-11-01T00:00:00Z",
            "comments_since": "2026-11-01T00:00:00Z",
            "etags": { "stale": "\"x\"" }
        },
        "items": {},
        "waiting": [],
        "polls": 3
    });
    // The saved state is in the single-cursor form older versions wrote.
    rig.tracker.seed_saved(old);
    rig.gh
        .on("GET", "/repos/GetFelix/other/issues?", ok(json!([])));
    rig.list(vec![]);
    ingest.poll(false).await.unwrap();

    for repo in ["GetFelix/felix", "GetFelix/other"] {
        let url = rig
            .gh
            .sent()
            .into_iter()
            .find(|r| r.url.contains(&format!("/repos/{repo}/issues?")))
            .unwrap()
            .url;
        assert!(url.contains("since=2026-11-01T00:00:00Z"), "{repo}: {url}");
        assert!(url.contains("state=all"), "{repo}: {url}");
    }
    let saved = rig.tracker.saved_state();
    assert!(saved.cursor.is_none());
    assert_eq!(saved.cursors.len(), 2);
    assert_eq!(saved.polls, 4);
    // The form written now has no legacy field to migrate again.
    let raw = rig.tracker.saved_raw();
    assert!(raw.get("cursor").is_none(), "{raw}");
}

// ---- pull requests and checks ----

const PR: &str = "GetFelix/felix#2210";

fn pull(edit: impl FnOnce(&mut Value)) -> Value {
    let mut v = fixture("pull");
    v["head"] = json!({ "sha": "abc123", "ref": pr_branch(&item_id(UP)) });
    v["base"] = json!({ "ref": "main", "repo": { "default_branch": "main" } });
    v["merged_by"] = json!({ "login": "gabriel" });
    // Created after the Fake tracker's clock starts, so after the first claim.
    v["created_at"] = json!("2027-01-15T09:00:00Z");
    v["requested_reviewers"] = json!([]);
    v["requested_teams"] = json!([]);
    edit(&mut v);
    v
}

fn run(name: &str, conclusion: Option<&str>) -> Value {
    json!({
        "name": name,
        "status": if conclusion.is_some() { "completed" } else { "in_progress" },
        "conclusion": conclusion,
        "html_url": format!("https://github.com/GetFelix/felix/actions/runs/{name}"),
    })
}

fn review(user: &str, state: &str, assoc: &str) -> Value {
    json!({ "user": { "login": user }, "state": state, "author_association": assoc })
}

impl Rig {
    /// An imported root whose agent reported the pull request.
    async fn with_pr(&self) -> (Ingest<Github<Script>, Fake>, kbl_core::names::ItemId) {
        let ingest = self.ingest(json!([admit_rule()]));
        self.list(vec![issue(812, |_| {})]);
        ingest.poll(false).await.unwrap();
        let item = self.hold(UP, Some(PR));
        (ingest, item)
    }

    /// The agent finishes its attempt, with the root's own items closed first.
    fn finish(&self, item: &kbl_core::names::ItemId) {
        for child in self.tracker.state().items[item].children.clone() {
            self.tracker.person(WorkRecord::Closed {
                item: child,
                why: kbl_core::work::CloseWhy::Dismissed,
            });
        }
        self.tracker.agent(
            "w1",
            WorkRecord::Done {
                item: item.clone(),
                attempt: 1,
                cost_micros: 0,
            },
        );
    }

    fn pr(&self, pull: Value, reviews: Vec<Value>, runs: Vec<Value>) {
        self.gh.on("GET", "/pulls/2210", ok(pull));
        self.gh
            .on("GET", "/pulls/2210/reviews", ok(Value::Array(reviews)));
        self.gh.on(
            "GET",
            "/status?",
            ok(json!({ "total_count": 0, "statuses": [] })),
        );
        self.gh.on(
            "GET",
            "/check-runs",
            ok(json!({ "total_count": runs.len(), "check_runs": runs })),
        );
    }
}

#[tokio::test]
async fn a_red_check_sends_the_item_to_needs_you_with_its_name_and_url() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    rig.pr(
        pull(|_| {}),
        vec![],
        vec![run("unit", Some("success")), run("lint", Some("failure"))],
    );
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    assert_eq!(s.stage(&item), Some(Stage::NeedsYou));
    assert_eq!(s.needs_you(&item), Some(NeedsYou::CheckFailed));
    let failing = s.items[&item]
        .pr_status
        .as_ref()
        .unwrap()
        .failing
        .clone()
        .unwrap();
    assert_eq!(failing.name, "lint");
    assert!(failing.url.ends_with("/lint"));
    assert_eq!(rig.tracker.count("pr_status"), 1);

    // The same answer again writes nothing.
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("pr_status"), 1);

    // The check is fixed: the item is back in review, with the CI wording.
    rig.pr(
        pull(|_| {}),
        vec![],
        vec![run("unit", Some("success")), run("lint", Some("success"))],
    );
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    assert_eq!(s.stage(&item), Some(Stage::InReview));
    assert_eq!(s.pr_waits(&item), Some(PrWaits::CiGreenNoReviewer));
}

#[tokio::test]
async fn only_required_checks_count_when_the_branch_protection_is_readable() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.gh.on(
        "GET",
        "/protection/required_status_checks",
        ok(json!({ "contexts": ["unit"], "checks": [] })),
    );
    rig.pr(
        pull(|_| {}),
        vec![],
        vec![
            run("unit", Some("success")),
            run("nightly", Some("failure")),
        ],
    );
    ingest.poll(false).await.unwrap();
    let st = rig.tracker.state().items[&item].pr_status.clone().unwrap();
    assert_eq!(st.checks, PrChecks::Green);
}

#[tokio::test]
async fn green_and_approved_is_approved_not_merged_and_a_merge_is_done() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    // A pending check, a reviewer asked: no special wording yet.
    rig.pr(
        pull(|p| p["requested_reviewers"] = json!([{ "login": "bob" }])),
        vec![],
        vec![run("unit", None)],
    );
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    let st = s.items[&item].pr_status.clone().unwrap();
    assert_eq!(
        (st.checks, st.review),
        (PrChecks::Pending, PrReview::Requested)
    );
    assert_eq!(s.pr_waits(&item), None);

    // Approved by a maintainer; a stranger's changes-requested does not count.
    rig.pr(
        pull(|_| {}),
        vec![
            review("bob", "APPROVED", "MEMBER"),
            review("eve", "CHANGES_REQUESTED", "NONE"),
        ],
        vec![run("unit", Some("success"))],
    );
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    assert_eq!(s.pr_waits(&item), Some(PrWaits::ApprovedNotMerged));
    assert_eq!(s.stage(&item), Some(Stage::InReview));

    // Merged: the item is done.
    rig.pr(
        pull(|p| {
            p["state"] = json!("closed");
            p["merged"] = json!(true);
        }),
        vec![],
        vec![],
    );
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    assert_eq!(s.items[&item].closed, Some(Closure::Merged));
    assert_eq!(s.stage(&item), Some(Stage::Done));
    // It stays done and is not polled for a state again.
    let before = rig.tracker.kinds().len();
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.kinds().len(), before);
}

#[tokio::test]
async fn changes_requested_beats_an_approval_and_a_dismissed_review_stops_counting() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.pr(
        pull(|_| {}),
        vec![
            review("bob", "APPROVED", "MEMBER"),
            review("carol", "CHANGES_REQUESTED", "COLLABORATOR"),
        ],
        vec![run("unit", Some("success"))],
    );
    ingest.poll(false).await.unwrap();
    let st = rig.tracker.state().items[&item].pr_status.clone().unwrap();
    assert_eq!(st.review, PrReview::ChangesRequested);
    rig.pr(
        pull(|_| {}),
        vec![
            review("bob", "APPROVED", "MEMBER"),
            review("carol", "CHANGES_REQUESTED", "COLLABORATOR"),
            review("carol", "DISMISSED", "COLLABORATOR"),
        ],
        vec![run("unit", Some("success"))],
    );
    ingest.poll(false).await.unwrap();
    let st = rig.tracker.state().items[&item].pr_status.clone().unwrap();
    assert_eq!(st.review, PrReview::Approved);
}

#[tokio::test]
async fn a_pull_request_by_someone_else_is_not_read_and_writes_nothing() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.pr(
        pull(|p| p["user"]["login"] = json!("mallory")),
        vec![review("bob", "APPROVED", "MEMBER")],
        vec![run("lint", Some("failure"))],
    );
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("pr_status"), 0);
    assert!(rig.tracker.state().items[&item].pr_status.is_none());
    // Nothing past the pull itself was requested for it.
    assert!(
        !rig.gh
            .sent()
            .iter()
            .any(|r| r.url.contains("/reviews") || r.url.contains("/check-runs"))
    );

    // Merged by a stranger does not close the item either.
    rig.pr(
        pull(|p| {
            p["user"]["login"] = json!("mallory");
            p["state"] = json!("closed");
            p["merged"] = json!(true);
        }),
        vec![],
        vec![],
    );
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.state().items[&item].closed, None);
}

#[tokio::test]
async fn a_pull_request_in_another_repo_is_not_read() {
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    rig.list(vec![issue(812, |_| {})]);
    ingest.poll(false).await.unwrap();
    rig.hold(UP, Some("Evil/repo#1"));
    ingest.poll(false).await.unwrap();
    assert!(!rig.gh.sent().iter().any(|r| r.url.contains("/pulls/")));
}

#[tokio::test]
async fn a_pull_request_that_fails_to_read_does_not_stop_the_poll() {
    let rig = rig();
    let (ingest, _) = rig.with_pr().await;
    rig.gh.on("GET", "/pulls/2210", status(500, &[]));
    rig.list(vec![at(issue(813, |_| {}), "2026-11-10T12:00:00Z")]);
    ingest.poll(false).await.unwrap();
    assert!(
        rig.tracker
            .state()
            .imported()
            .any(|(_, u, _)| u == "GetFelix/felix#813")
    );
}

#[tokio::test]
async fn an_old_merged_request_swapped_in_by_an_agent_does_not_close_the_root() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    // The reported request is merged, ours, in the same repo, but is about
    // another branch and another issue.
    rig.pr(
        pull(|p| {
            p["head"]["ref"] = json!("fix/something-else");
            p["body"] = json!("Fixes #3");
            p["state"] = json!("closed");
            p["merged"] = json!(true);
        }),
        vec![],
        vec![],
    );
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("pr_status"), 0);
    assert_eq!(rig.tracker.state().items[&item].closed, None);

    // Named by a closing reference to the issue, the same request is accepted.
    rig.pr(
        pull(|p| {
            p["head"]["ref"] = json!("fix/something-else");
            p["body"] = json!("Fixes #812.");
            p["state"] = json!("closed");
            p["merged"] = json!(true);
        }),
        vec![],
        vec![],
    );
    ingest.poll(false).await.unwrap();
    assert_eq!(
        rig.tracker.state().items[&item].closed,
        Some(Closure::Merged)
    );
}

#[test]
fn only_a_closing_keyword_before_the_issue_links_a_request() {
    assert!(closes_issue("Closes #812", UP));
    assert!(closes_issue("fixes: GetFelix/felix#812.", UP));
    assert!(!closes_issue("see #812", UP));
    assert!(!closes_issue("Fixes #8123", UP));
    assert!(!closes_issue("Fixes #81", UP));
}

#[tokio::test]
async fn flipping_the_reported_request_away_and_back_restores_a_red_status() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.finish(&item);
    rig.pr(pull(|_| {}), vec![], vec![run("lint", Some("failure"))]);
    ingest.poll(false).await.unwrap();
    assert_eq!(
        rig.tracker.state().needs_you(&item),
        Some(NeedsYou::CheckFailed)
    );
    // The agent points `pr` elsewhere and back; the fold forgot the status
    // when it changed, and ingest has not polled in between.
    for pr in ["GetFelix/felix#2211", PR] {
        rig.tracker.person(WorkRecord::Progress {
            item: item.clone(),
            attempt: 1,
            note: kbl_core::content::Content::Text("pr".into()),
            branch: None,
            pr: Some(pr.into()),
        });
    }
    assert!(rig.tracker.state().items[&item].pr_status.is_none());
    ingest.poll(false).await.unwrap();
    assert_eq!(
        rig.tracker.state().needs_you(&item),
        Some(NeedsYou::CheckFailed)
    );
}

#[tokio::test]
async fn an_old_merged_request_with_the_matching_branch_created_before_the_claim_is_refused() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    // The branch matches what the agent reported, and the body even names the
    // issue, but the request is older than the first claim.
    rig.pr(
        pull(|p| {
            p["created_at"] = json!("2026-11-01T00:00:00Z");
            p["body"] = json!("Fixes #812");
            p["state"] = json!("closed");
            p["merged"] = json!(true);
        }),
        vec![],
        vec![],
    );
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("pr_status"), 0);
    assert_eq!(rig.tracker.state().items[&item].closed, None);
}

#[tokio::test]
async fn a_merge_into_another_branch_or_by_our_own_login_does_not_close_the_root() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    rig.pr(
        pull(|p| {
            p["state"] = json!("closed");
            p["merged"] = json!(true);
            p["base"]["ref"] = json!("release-1");
        }),
        vec![],
        vec![],
    );
    ingest.poll(false).await.unwrap();
    let s = rig.tracker.state();
    assert_eq!(s.items[&item].closed, None);
    assert_eq!(
        s.items[&item].pr_status.as_ref().unwrap().phase,
        kbl_core::work::PrPhase::Closed
    );

    // Merged into the default branch, but by the credential's own login.
    rig.pr(
        pull(|p| {
            p["state"] = json!("closed");
            p["merged"] = json!(true);
            p["merged_by"] = json!({ "login": "kbl[bot]" });
        }),
        vec![],
        vec![],
    );
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.state().items[&item].closed, None);

    // A person merging it into the default branch is the real thing.
    rig.pr(
        pull(|p| {
            p["state"] = json!("closed");
            p["merged"] = json!(true);
        }),
        vec![],
        vec![],
    );
    ingest.poll(false).await.unwrap();
    assert_eq!(
        rig.tracker.state().items[&item].closed,
        Some(Closure::Merged)
    );
}

#[tokio::test]
async fn the_branch_the_agent_reported_is_not_a_link() {
    let rig = rig();
    let (ingest, item) = rig.with_pr().await;
    // The head branch is the one the agent reported in its progress, not the
    // one the tracker derives, and the body does not name the issue.
    rig.pr(
        pull(|p| {
            p["head"]["ref"] = json!("fix/stale-read");
            p["state"] = json!("closed");
            p["merged"] = json!(true);
        }),
        vec![],
        vec![],
    );
    ingest.poll(false).await.unwrap();
    assert_eq!(rig.tracker.count("pr_status"), 0);
    assert_eq!(rig.tracker.state().items[&item].closed, None);
}
