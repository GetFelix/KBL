use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::*;
use crate::testing::{Fake, Script, issue, labels, ok, status};
use kbl_core::work::WorkRecord;

const API: &str = "https://api.github.com";

fn config(extra_rules: Vec<Value>) -> Config {
    let mut rules = vec![
        json!({ "name": "first", "repo": "GetFelix/felix", "labels_all": ["agent-ok"],
                "mode": "admit", "cap": 1, "target_repo": "GetFelix/other" }),
        json!({ "name": "wide", "repo": "GetFelix/felix", "labels_any": ["p1", "docs"],
                "mode": "propose" }),
    ];
    rules.extend(extra_rules);
    Config::parse(
        &json!({
            "tracker": { "url": "http://t", "token_env": "TRACKER_TOKEN" },
            "github": { "token_env": "GH_TOKEN", "own_logins": ["kbl[bot]"] },
            "rules": rules,
        })
        .to_string(),
    )
    .unwrap()
}

fn github(gh: &Script) -> Github<ReadOnly<Script>> {
    Github::new(API, ReadOnly(gh.clone()), vec![])
}

fn listing(gh: &Script) {
    let mut pr = issue(900, |_| {});
    pr["pull_request"] = json!({ "url": "x" });
    gh.on(
        "GET",
        "/issues?",
        ok(json!([
            issue(1, |v| labels(v, &["agent-ok", "p1"])),
            issue(2, |v| labels(v, &["agent-ok"])),
            issue(3, |v| labels(v, &["docs"])),
            issue(4, |v| labels(v, &["unrelated"])),
            pr,
        ])),
    );
}

fn pick<'a>(r: &'a [RuleReport], name: &str) -> &'a RuleReport {
    r.iter().find(|r| r.rule == name).unwrap()
}

fn temp(name: &str, text: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("kbl-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

#[tokio::test]
async fn the_dry_run_gives_each_issue_to_the_first_matching_rule_and_applies_the_cap() {
    let gh = Script::default();
    listing(&gh);
    let cfg = config(vec![]);
    let mut report = Report::default();
    dry_run(&cfg, &github(&gh), None::<&Fake>, &mut report).await;

    let first = pick(&report.rules, "first");
    // #1 and #2 both match `first`; its cap of 1 lets the older one in.
    assert_eq!(first.would_import.len(), 1);
    assert_eq!(first.would_import[0].issue, "GetFelix/felix#1");
    assert_eq!(first.over_cap.len(), 1);
    assert_eq!(first.over_cap[0].issue, "GetFelix/felix#2");
    // The priority label sets the tier only where the rule maps it; the target is the rule's.
    assert_eq!(
        first.would_import[0].target.as_deref(),
        Some("GetFelix/other")
    );

    // #1 also carries p1 but belongs to `first`; only #3 reaches `wide`.
    let wide = pick(&report.rules, "wide");
    assert_eq!(
        wide.would_import
            .iter()
            .map(|p| p.issue.as_str())
            .collect::<Vec<_>>(),
        ["GetFelix/felix#3"]
    );
    assert_eq!(wide.mode, "propose");
    assert_eq!(report.unmatched, 1);
    // The pull request in the listing is not an issue.
    assert!(gh.writes().is_empty());
}

#[tokio::test]
async fn the_dry_run_counts_roots_the_tracker_already_has() {
    let gh = Script::default();
    listing(&gh);
    let tracker = Fake::default();
    tracker
        .append(&WorkRecord::Imported {
            item: crate::sync::item_id("GetFelix/felix#1"),
            source: "github".into(),
            upstream_id: "GetFelix/felix#1".into(),
            title: "t".into(),
        })
        .await
        .unwrap();
    let before = tracker.kinds();
    // The worker's saved state says `first` admitted #1.
    tracker.seed_saved(json!({
        "items": { "GetFelix/felix#1": {
            "item": crate::sync::item_id("GetFelix/felix#1"), "rule": "first",
            "fingerprint": "f", "title": "t", "in_scope": true } },
        "waiting": [],
    }));
    let mut report = Report::default();
    dry_run(&config(vec![]), &github(&gh), Some(&tracker), &mut report).await;

    let first = pick(&report.rules, "first");
    assert_eq!(first.already_imported, 1);
    assert_eq!(first.open_now, 1);
    // The rule is at its cap of 1 with #1, so #2 waits.
    assert!(first.would_import.is_empty());
    assert_eq!(first.over_cap.len(), 1);
    // Nothing was written to the tracker.
    assert_eq!(tracker.kinds(), before);
    assert_eq!(tracker.saved_raw()["items"].as_object().unwrap().len(), 1);
}

#[tokio::test]
async fn a_repo_the_credential_cannot_list_is_an_error() {
    let gh = Script::default();
    gh.on("GET", "/issues?", status(404, &[]));
    let mut report = Report::default();
    dry_run(&config(vec![]), &github(&gh), None::<&Fake>, &mut report).await;
    let f = report.finish();
    assert!(!f.ok);
    assert!(
        f.findings
            .iter()
            .any(|f| f.scope == "repo GetFelix/felix" && f.message.contains("404"))
    );
}

#[tokio::test]
async fn check_sends_no_write_to_the_vendor() {
    let gh = Script::default();
    listing(&gh);
    gh.on("GET", "/user", ok(json!({ "login": "kbl-bot" })));
    gh.on(
        "GET",
        "/repos/GetFelix/felix",
        ok(json!({ "permissions": { "pull": true, "push": true } })),
    );
    let cfg = config(vec![
        json!({ "name": "pre", "repo": "GetFelix/felix", "labels_all": ["x"],
                                  "mode": "admit", "open_prerequisites": true }),
    ]);
    let g = cfg.github.as_ref().unwrap();
    let mut report = Report::default();
    github_findings(&cfg, g, &github(&gh), &mut report.findings).await;
    dry_run(&cfg, &github(&gh), None::<&Fake>, &mut report).await;
    assert!(gh.writes().is_empty(), "{:?}", gh.writes());
    assert!(!gh.sent().is_empty());
    // And a write cannot leave through the guard even if code tried.
    let guarded = ReadOnly(gh.clone());
    assert!(
        guarded
            .send(Req::post("https://api.github.com/x".into(), json!({})))
            .await
            .is_err()
    );
    assert!(gh.writes().is_empty());
}

#[tokio::test]
async fn permissions_are_reported_per_rule() {
    let cfg = config(vec![
        json!({ "name": "pre", "repo": "GetFelix/felix", "labels_all": ["x"],
                                  "mode": "admit", "open_prerequisites": true }),
    ]);
    let g = cfg.github.as_ref().unwrap();
    let run = |perms: Value| {
        let cfg = &cfg;
        async move {
            let gh = Script::default();
            gh.on("GET", "/user", ok(json!({ "login": "me" })));
            gh.on(
                "GET",
                "/repos/GetFelix/felix",
                ok(json!({ "permissions": perms })),
            );
            let mut out = Vec::new();
            github_findings(cfg, g, &github(&gh), &mut out).await;
            out
        }
    };
    let find = |out: &[Finding], rule: &str, level: Level| {
        out.iter()
            .any(|f| f.scope == format!("rule {rule}") && f.level == level)
    };
    let read_only = run(json!({ "pull": true, "push": false, "triage": false })).await;
    assert!(find(&read_only, "pre", Level::Error), "{read_only:?}");
    assert!(find(&read_only, "first", Level::Warn));
    // Triage is enough for issues, not for pull requests.
    let triage = run(json!({ "pull": true, "push": false, "triage": true })).await;
    assert!(find(&triage, "pre", Level::Ok));
    assert!(find(&triage, "pre", Level::Warn));
    let write = run(json!({ "pull": true, "push": true })).await;
    assert!(
        !write
            .iter()
            .any(|f| f.level == Level::Error || f.level == Level::Warn),
        "{write:?}"
    );
    // GitHub gave no summary: unknown is a warning, not an error.
    let unknown = run(Value::Null).await;
    assert!(find(&unknown, "pre", Level::Warn));
    assert!(!unknown.iter().any(|f| f.level == Level::Error));
}

#[tokio::test]
async fn a_rejected_credential_and_a_hidden_repo_are_errors() {
    let cfg = config(vec![]);
    let g = cfg.github.as_ref().unwrap();
    let gh = Script::default();
    gh.on("GET", "/user", status(401, &[]));
    let mut out = Vec::new();
    github_findings(&cfg, g, &github(&gh), &mut out).await;
    assert!(
        out.iter()
            .any(|f| f.level == Level::Error && f.message.contains("401"))
    );

    let gh = Script::default();
    gh.on("GET", "/user", ok(json!({ "login": "me" })));
    let mut out = Vec::new();
    github_findings(&cfg, g, &github(&gh), &mut out).await;
    assert!(
        out.iter()
            .any(|f| f.level == Level::Error && f.scope == "repo GetFelix/felix")
    );
}

fn app_config() -> Config {
    Config::parse(
        &json!({
            "tracker": { "url": "http://t", "token_env": "TRACKER_TOKEN" },
            "github": { "app": { "id": "1", "installation_id": 2,
                                 "private_key_file": "/nonexistent.pem" } },
            "rules": [{ "name": "r", "repo": "GetFelix/felix", "labels_all": ["agent-ok"], "mode": "admit" }],
        })
        .to_string(),
    )
    .unwrap()
}

#[tokio::test]
async fn a_token_credential_refused_at_user_fails_sign_in_but_an_app_passes() {
    for status_code in [401, 403, 404] {
        let cfg = config(vec![]);
        let gh = Script::default();
        gh.on("GET", "/user", status(status_code, &[]));
        let mut out = Vec::new();
        github_findings(&cfg, cfg.github.as_ref().unwrap(), &github(&gh), &mut out).await;
        assert!(
            out.iter().any(|f| f.level == Level::Error
                && f.scope == "github"
                && f.message.contains(&status_code.to_string())),
            "{status_code}: {out:?}"
        );
        assert!(!out.iter().any(|f| f.message.contains("no user")));
    }
    let cfg = app_config();
    for status_code in [403, 404] {
        let gh = Script::default();
        gh.on("GET", "/user", status(status_code, &[]));
        let mut out = Vec::new();
        github_findings(&cfg, cfg.github.as_ref().unwrap(), &github(&gh), &mut out).await;
        assert!(
            out.iter()
                .any(|f| f.level == Level::Ok && f.message.contains("no user")),
            "{out:?}"
        );
    }
}

fn jira_config() -> Config {
    Config::parse(
        &json!({
            "tracker": { "url": "http://t", "token_env": "TRACKER_TOKEN" },
            "jira": { "site": "acme", "api": "https://acme.atlassian.net",
                      "email": "kbl@acme.example", "token_env": "JIRA_TOKEN" },
            "rules": [{ "name": "agent-ok", "repo": "acme/FEL", "jql": "labels = agent-ok",
                        "mode": "admit", "comment": true, "link_pr": true,
                        "transition_on_pr": "In Review", "target_repo": "GetFelix/felix" }],
        })
        .to_string(),
    )
    .unwrap()
}

fn jira(site: &Script, cfg: &Config) -> Jira<ReadOnly<Script>> {
    let j = cfg.jira.as_ref().unwrap();
    Jira::new(
        &j.site,
        &j.api,
        ReadOnly(site.clone()),
        vec![],
        crate::jira_scopes(cfg),
        &j.pr_url_base,
    )
}

fn raw_jira(key: &str) -> Value {
    json!({
        "key": key,
        "self": "https://acme.atlassian.net/rest/api/2/issue/10001",
        "fields": {
            "summary": "Stale read after failover", "description": "Reads go stale.",
            "status": { "name": "To Do", "statusCategory": { "key": "new" } },
            "resolution": null, "labels": ["agent-ok"], "priority": { "name": "High" },
            "components": [], "issuetype": { "name": "Bug" },
            "reporter": { "displayName": "Alice" }, "assignee": null, "fixVersions": [],
            "updated": "2026-11-10T12:00:05.000+0000"
        }
    })
}

#[tokio::test]
async fn the_jira_dry_run_follows_the_jql_scope_and_writes_nothing() {
    let site = Script::default();
    site.on(
        "GET",
        "/search/jql",
        ok(json!({ "issues": [raw_jira("FEL-12")], "isLast": true })),
    );
    let cfg = jira_config();
    let jira = jira(&site, &cfg);
    let mut report = Report::default();
    dry_run(&cfg, &jira, None::<&Fake>, &mut report).await;
    let r = pick(&report.rules, "agent-ok");
    assert_eq!(r.would_import.len(), 1);
    assert_eq!(r.would_import[0].issue, "acme/FEL#12");
    // High maps to tier 1; no component maps, so target_repo is the work repo.
    assert_eq!(r.would_import[0].tier, 1);
    assert_eq!(r.would_import[0].target.as_deref(), Some("GetFelix/felix"));
    assert!(site.writes().is_empty());
}

#[tokio::test]
async fn jira_permissions_follow_what_each_rule_turns_on() {
    let cfg = jira_config();
    let run = |comment_ok: bool| {
        let cfg = &cfg;
        async move {
            let site = Script::default();
            site.on(
                "GET",
                "/myself",
                ok(json!({ "displayName": "KBL", "accountId": "a1" })),
            );
            site.on(
                "GET",
                "/mypermissions",
                ok(json!({ "permissions": {
                    "BROWSE_PROJECTS": { "havePermission": true },
                    "ADD_COMMENTS": { "havePermission": comment_ok },
                    "LINK_ISSUES": { "havePermission": true },
                    "TRANSITION_ISSUES": { "havePermission": true },
                } })),
            );
            let mut jira = jira(&site, cfg);
            let mut out = Vec::new();
            jira_findings(cfg, &mut jira, &mut out).await;
            assert!(site.writes().is_empty());
            out
        }
    };
    let good = run(true).await;
    assert!(!good.iter().any(|f| f.level == Level::Error), "{good:?}");
    let bad = run(false).await;
    let e: Vec<_> = bad.iter().filter(|f| f.level == Level::Error).collect();
    assert_eq!(e.len(), 1, "{bad:?}");
    assert!(e[0].message.contains("add comments"));
}

#[tokio::test]
async fn a_jira_credential_that_cannot_name_itself_needs_own_logins() {
    let cfg = jira_config();
    let site = Script::default();
    site.on("GET", "/myself", status(401, &[]));
    let mut jira = jira(&site, &cfg);
    let mut out = Vec::new();
    jira_findings(&cfg, &mut jira, &mut out).await;
    assert!(
        out.iter()
            .any(|f| f.level == Level::Error && f.message.contains("refused"))
    );
    assert!(
        out.iter()
            .any(|f| f.level == Level::Error && f.message.contains("own_logins"))
    );
}

#[tokio::test]
async fn a_config_the_worker_would_refuse_is_one_error() {
    let cases = [
        (
            json!({ "tracker": { "url": "u", "token_env": "T" }, "github": { "token_env": "G" },
                 "rules": [], "bogus": 1 }),
            "bogus",
        ),
        (
            json!({ "tracker": { "url": "u", "token_env": "T" }, "github": { "token_env": "G" },
                 "rules": [] }),
            "no intake rules",
        ),
        (
            json!({ "tracker": { "url": "u", "token_env": "T" },
                 "github": { "token_env": "G" },
                 "jira": { "site": "a", "api": "x", "email": "e", "token_env": "J" },
                 "rules": [{ "name": "r", "repo": "o/n", "mode": "admit" }] }),
            "exactly one of github, jira, ado and gitlab",
        ),
        (
            json!({ "tracker": { "url": "u", "token_env": "T" }, "github": { "token_env": "G" },
                 "rules": [{ "name": "r", "repo": "nope", "mode": "admit" }] }),
            "owner/name",
        ),
        (
            json!({ "tracker": { "url": "u", "token_env": "T" },
                 "jira": { "site": "a", "api": "x", "email": "e", "token_env": "J",
                           "oauth_token_file": "/f" },
                 "rules": [{ "name": "r", "repo": "a/P", "mode": "admit", "jql": "x" }] }),
            "signs in with",
        ),
    ];
    for (i, (cfg, want)) in cases.iter().enumerate() {
        let path = temp(&format!("bad-{i}.json"), &cfg.to_string());
        let report = report_for(&path).await;
        assert!(!report.ok);
        assert_eq!(report.findings.len(), 1);
        assert!(
            report.findings[0].message.contains(want),
            "{:?}",
            report.findings
        );
    }
}

#[tokio::test]
async fn a_missing_config_file_is_an_error() {
    let report = report_for(Path::new("/nonexistent/ingest.json")).await;
    assert!(!report.ok);
}

#[test]
fn credentials_are_named_never_shown() {
    let cfg = config(vec![]);
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let env = |n: &str| match n {
        "GH_TOKEN" | "TRACKER_TOKEN" => Some(secret.to_owned()),
        _ => None,
    };
    let all = static_findings(&cfg, &env);
    assert!(all.iter().all(|f| !f.message.contains(secret)));
    assert!(all.iter().any(|f| f.message.contains("GH_TOKEN is set")));
    let none = static_findings(&cfg, &|_| None);
    let missing: Vec<_> = none.iter().filter(|f| f.level == Level::Error).collect();
    assert_eq!(missing.len(), 2);
    assert!(missing.iter().any(|f| f.message.contains("TRACKER_TOKEN")));
}

#[test]
fn a_token_file_is_checked_without_being_read_out() {
    let secret = "supersecretvalue";
    let good = temp("token", secret);
    let empty = temp("empty-token", "  \n");
    let cfg = |p: &Path| {
        Config::parse(
            &json!({
                "tracker": { "url": "http://t", "token_env": "T" },
                "github": { "token_file": p, "token_env": "G" },
                "rules": [{ "name": "r", "repo": "o/n", "labels_all": ["a"], "mode": "admit" }],
            })
            .to_string(),
        )
        .unwrap()
    };
    let env = |_: &str| Some("x".to_owned());
    let ok_out = static_findings(&cfg(&good), &env);
    assert!(ok_out.iter().all(|f| !f.message.contains(secret)));
    assert!(
        ok_out
            .iter()
            .any(|f| f.level == Level::Warn && f.message.contains("several credentials"))
    );
    let empty_out = static_findings(&cfg(&empty), &env);
    assert!(
        empty_out
            .iter()
            .any(|f| f.level == Level::Error && f.message.contains("is empty"))
    );
    let gone = static_findings(&cfg(Path::new("/nonexistent/t")), &env);
    assert!(
        gone.iter()
            .any(|f| f.level == Level::Error && f.message.contains("cannot be read"))
    );
}

#[test]
fn a_rule_with_no_filter_is_warned_about() {
    let cfg = Config::parse(
        &json!({
            "tracker": { "url": "http://t", "token_env": "T" },
            "github": { "token_env": "G" },
            "rules": [{ "name": "all", "repo": "o/n", "mode": "admit", "cap": 0 }],
        })
        .to_string(),
    )
    .unwrap();
    let out = static_findings(&cfg, &|_| Some("x".into()));
    let warns: Vec<_> = out.iter().filter(|f| f.level == Level::Warn).collect();
    assert_eq!(warns.len(), 2, "{out:?}");
}

#[test]
fn the_text_report_lists_picks_and_the_json_report_is_machine_readable() {
    let report = Report {
        findings: vec![Finding::new(Level::Error, "github", "bad")],
        rules: vec![RuleReport {
            rule: "r".into(),
            repo: "o/n".into(),
            mode: "admit",
            skips_triage: true,
            cap: None,
            open_now: 0,
            would_import: vec![Pick {
                issue: "o/n#1".into(),
                title: "t".into(),
                tier: 2,
                target: None,
            }],
            over_cap: vec![],
            already_imported: 0,
        }],
        unmatched: 3,
        ok: false,
    };
    let text = render(&report);
    assert!(text.contains("ERROR github: bad"));
    assert!(text.contains("import o/n#1 (tier 2, work in no repository: Needs you)"));
    assert!(text.contains("3 open issue(s) match no rule"));
    assert!(text.contains("admits straight to ready"));
    let v: Value = serde_json::to_value(&report).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["rules"][0]["would_import"][0]["issue"], "o/n#1");
    assert_eq!(v["rules"][0]["skips_triage"], true);
}

#[tokio::test]
async fn the_dry_run_skips_ingests_own_prerequisite_issues_and_other_sources_roots() {
    let gh = Script::default();
    let own = issue(5, |v| {
        labels(v, &["agent-ok"]);
        v["user"] = json!({ "login": "kbl[bot]", "type": "Bot" });
        v["body"] = json!("Needed.\n\nkbl-prerequisite: gabc");
    });
    gh.on(
        "GET",
        "/issues?",
        ok(json!([issue(1, |v| labels(v, &["agent-ok"])), own])),
    );
    let cfg = config(vec![
        json!({ "name": "pre", "repo": "GetFelix/felix", "labels_all": ["zzz"],
                                  "mode": "admit", "open_prerequisites": true }),
    ]);
    let tracker = Fake::default();
    // A root of another source under the same upstream id text.
    tracker
        .append(&WorkRecord::Imported {
            item: crate::sync::item_id("GetFelix/felix#1"),
            source: "jira".into(),
            upstream_id: "GetFelix/felix#1".into(),
            title: "t".into(),
        })
        .await
        .unwrap();
    let g = Github::new(API, ReadOnly(gh.clone()), vec!["kbl[bot]".into()]);
    let mut report = Report::default();
    dry_run(&cfg, &g, Some(&tracker), &mut report).await;
    let first = pick(&report.rules, "first");
    assert_eq!(first.already_imported, 0);
    assert_eq!(
        first
            .would_import
            .iter()
            .map(|p| p.issue.as_str())
            .collect::<Vec<_>>(),
        ["GetFelix/felix#1"]
    );
    assert!(first.over_cap.is_empty());
}

/// A vendor answered by a handler, with every request kept.
#[derive(Clone)]
struct Vendor {
    handler: Arc<dyn Fn(&Req) -> Res + Send + Sync>,
    sent: Arc<Mutex<Vec<Req>>>,
}

impl Vendor {
    fn new(handler: impl Fn(&Req) -> Res + Send + Sync + 'static) -> Self {
        Self {
            handler: Arc::new(handler),
            sent: Arc::default(),
        }
    }

    fn sent(&self) -> Vec<Req> {
        self.sent.lock().unwrap().clone()
    }

    /// Methods and paths of every request that is not a GET.
    fn non_gets(&self) -> Vec<String> {
        self.sent()
            .iter()
            .filter(|r| r.method != "GET")
            .map(|r| format!("{} {}", r.method, path(r)))
            .collect()
    }

    fn touched(&self, part: &str) -> bool {
        self.sent().iter().any(|r| path(r).contains(part))
    }
}

impl Transport for Vendor {
    async fn send(&self, req: Req) -> Result<Res> {
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

fn messages(findings: &[Finding], level: Level) -> Vec<String> {
    findings
        .iter()
        .filter(|f| f.level == level)
        .map(|f| format!("{}: {}", f.scope, f.message))
        .collect()
}

fn has(findings: &[Finding], level: Level, part: &str) -> bool {
    messages(findings, level).iter().any(|m| m.contains(part))
}

fn ids(report: &Report, rule: &str) -> Vec<String> {
    let mut v: Vec<String> = pick(&report.rules, rule)
        .would_import
        .iter()
        .map(|p| p.issue.clone())
        .collect();
    v.sort();
    v
}

/// What the worker's first poll imported, as the tracker holds it.
async fn imported(tracker: &Fake) -> Vec<String> {
    let mut v: Vec<String> = tracker
        .roots()
        .await
        .unwrap()
        .roots
        .into_iter()
        .map(|r| r.upstream_id)
        .collect();
    v.sort();
    v
}

fn ado_item(id: u64, tags: &str, state: &str) -> Value {
    json!({
        "id": id,
        "fields": {
            "System.TeamProject": "Platform Team",
            "System.Title": format!("Item {id}"),
            "System.Description": "text",
            "System.State": state,
            "System.Tags": tags,
            "System.WorkItemType": "Bug",
            "System.CreatedBy": { "displayName": "Alice" },
            "System.ChangedDate": format!("2026-11-10T12:00:0{id}.000Z"),
        },
        "relations": []
    })
}

fn ado_config(rule: Value) -> Config {
    let mut base = json!({ "name": "agent-ok", "repo": "contoso/Platform",
        "labels_all": ["agent-ok"], "mode": "admit" });
    base.as_object_mut()
        .unwrap()
        .extend(rule.as_object().unwrap().clone());
    Config::parse(
        &json!({
            "tracker": { "url": "http://t", "token_env": "T" },
            "ado": { "organization": "contoso", "pat_env": "ADO_PAT",
                     "projects": { "Platform": "Platform Team" } },
            "rules": [base],
        })
        .to_string(),
    )
    .unwrap()
}

/// An Azure DevOps organization. `write` is what the permission service
/// answers for work item edit: None is a refusal to say.
fn ado_org(items: Vec<Value>, write: Option<bool>) -> Vendor {
    Vendor::new(move |req| {
        let p = path(req);
        if p.ends_with("/_apis/connectionData") {
            return res(
                200,
                json!({ "authenticatedUser": { "providerDisplayName": "KBL Bot" } }),
            );
        }
        if p.contains("/wit/classificationnodes/areas") {
            return res(200, json!({ "identifier": "node-1" }));
        }
        if p.contains("/_apis/permissions/") {
            return match write {
                Some(w) => res(200, json!({ "value": [w] })),
                None => res(403, Value::Null),
            };
        }
        if p.ends_with("/_apis/wit/wiql") && req.method == "POST" {
            let listed: Vec<Value> = items.iter().map(|i| json!({ "id": i["id"] })).collect();
            return res(200, json!({ "workItems": listed }));
        }
        if p.ends_with("/_apis/wit/workitems") {
            return res(200, json!({ "value": items }));
        }
        if p.ends_with("/comments") {
            return res(200, json!({ "comments": [] }));
        }
        res(404, Value::Null)
    })
}

fn ado_source(org: &Vendor, cfg: &Config) -> Ado<ReadOnlyWiql<Vendor>> {
    let a = cfg.ado.as_ref().unwrap();
    Ado::new(
        &a.organization,
        &a.api,
        ReadOnlyWiql(org.clone()),
        vec![],
        crate::ado_scopes(cfg),
        &a.projects,
        &a.pr_url_base,
    )
}

#[tokio::test]
async fn ado_sign_in_and_permissions_are_reported_per_rule() {
    let org = ado_org(vec![], Some(true));
    let cfg = ado_config(json!({ "comment": true }));
    let mut out = Vec::new();
    ado_findings(&cfg, &mut ado_source(&org, &cfg), &mut out).await;
    assert!(has(&out, Level::Ok, "signs in as KBL Bot"), "{out:?}");
    assert!(has(&out, Level::Ok, "can read work items in Platform Team"));
    assert!(has(
        &out,
        Level::Ok,
        "work items: may edit in Platform Team"
    ));
    assert!(messages(&out, Level::Error).is_empty(), "{out:?}");
}

#[tokio::test]
async fn an_ado_credential_the_service_refuses_is_one_error_and_stops_the_probe() {
    // A bad personal access token is a 203 sign-in page, which the adapter reads as 401.
    let org = Vendor::new(|_| res(203, Value::Null));
    let cfg = ado_config(json!({}));
    let mut out = Vec::new();
    ado_findings(&cfg, &mut ado_source(&org, &cfg), &mut out).await;
    assert!(
        has(&out, Level::Error, "refused the credential (401)"),
        "{out:?}"
    );
    assert_eq!(out.len(), 1, "{out:?}");
    assert!(!org_touched_projects(&org));
}

fn org_touched_projects(v: &Vendor) -> bool {
    v.touched("classificationnodes")
}

#[tokio::test]
async fn a_project_the_ado_credential_cannot_read_is_an_error() {
    let org = Vendor::new(|req| {
        if path(req).ends_with("/_apis/connectionData") {
            res(
                200,
                json!({ "authenticatedUser": { "providerDisplayName": "KBL" } }),
            )
        } else {
            res(404, Value::Null)
        }
    });
    let cfg = ado_config(json!({}));
    let mut out = Vec::new();
    ado_findings(&cfg, &mut ado_source(&org, &cfg), &mut out).await;
    assert!(has(&out, Level::Error, "repo contoso/Platform"), "{out:?}");
    assert!(has(
        &out,
        Level::Error,
        "answered 404 reading project Platform Team"
    ));
}

#[tokio::test]
async fn a_missing_ado_write_right_is_an_error_only_where_a_rule_writes() {
    let cfg = ado_config(json!({ "comment": true, "transition_on_pr": "Resolved" }));
    let org = ado_org(vec![], Some(false));
    let mut out = Vec::new();
    ado_findings(&cfg, &mut ado_source(&org, &cfg), &mut out).await;
    assert!(
        has(
            &out,
            Level::Error,
            "may not edit work items in Platform Team"
        ),
        "{out:?}"
    );

    // The permission service saying nothing is a warning, not a verdict.
    let org = ado_org(vec![], None);
    let mut out = Vec::new();
    ado_findings(&cfg, &mut ado_source(&org, &cfg), &mut out).await;
    assert!(has(&out, Level::Warn, "could not tell whether"), "{out:?}");
    assert!(messages(&out, Level::Error).is_empty());

    // A read-only rule never asks, so a read-only credential passes.
    let cfg = ado_config(json!({}));
    let org = ado_org(vec![], Some(false));
    let mut out = Vec::new();
    ado_findings(&cfg, &mut ado_source(&org, &cfg), &mut out).await;
    assert!(messages(&out, Level::Error).is_empty(), "{out:?}");
    assert!(!org.touched("/permissions/"));
}

#[tokio::test]
async fn the_ado_guard_lets_through_one_query_and_nothing_else() {
    let org = ado_org(vec![], None);
    let guard = ReadOnlyWiql(org.clone());
    let q = json!({ "query": "SELECT [System.Id] FROM WorkItems" });
    let base = "https://dev.azure.com/contoso/Platform%20Team/_apis";
    let post = |url: String, body: Value| Req::post(url, body);
    assert!(
        guard
            .send(post(format!("{base}/wit/wiql?api-version=7.1"), q.clone()))
            .await
            .is_ok()
    );
    let id = "0b9e1d3e-7c1a-4f0e-9a52-5f2f0c0d1e11";
    let refused = [
        post(
            format!("{base}/wit/workItems/1/comments"),
            json!({ "text": "x" }),
        ),
        post(format!("{base}/wit/wiql/{id}"), q.clone()),
        post(format!("{base}/wit/wiql/../workitems"), q.clone()),
        post(format!("{base}/wit/wiql"), json!({ "query": "DELETE" })),
        post(
            format!("{base}/wit/wiql"),
            json!({ "query": "SELECT x", "extra": 1 }),
        ),
        post(format!("{base}/wit/wiql"), json!([])),
        Req {
            method: "PATCH",
            url: format!("{base}/wit/wiql"),
            if_none_match: None,
            body: Some(q.clone()),
        },
    ];
    for req in refused {
        let what = format!("{} {}", req.method, req.url);
        assert!(guard.send(req).await.is_err(), "{what}");
    }
    // Only the one query reached the organization.
    assert_eq!(org.non_gets().len(), 1, "{:?}", org.non_gets());
}

#[tokio::test]
async fn the_ado_check_sends_only_reads_and_the_query() {
    let org = ado_org(vec![ado_item(1, "agent-ok", "Active")], Some(true));
    let cfg =
        ado_config(json!({ "comment": true, "link_pr": true, "transition_on_pr": "Resolved" }));
    let mut report = Report::default();
    let ado = ado_source(&org, &cfg);
    dry_run(&cfg, &ado, None::<&Fake>, &mut report).await;
    ado_findings(&cfg, &mut ado_source(&org, &cfg), &mut report.findings).await;
    assert_eq!(
        org.non_gets(),
        ["POST /contoso/Platform%20Team/_apis/wit/wiql"]
    );
}

#[tokio::test]
async fn the_ado_dry_run_picks_what_the_worker_imports() {
    let items = vec![
        ado_item(1, "agent-ok", "Active"),
        ado_item(2, "agent-ok; storage", "New"),
        ado_item(3, "other", "Active"),
        ado_item(4, "agent-ok", "Active"),
    ];
    let cfg = ado_config(json!({ "cap": 2 }));
    let org = ado_org(items.clone(), None);
    let mut report = Report::default();
    dry_run(&cfg, &ado_source(&org, &cfg), None::<&Fake>, &mut report).await;
    let planned = ids(&report, "agent-ok");
    assert_eq!(planned.len(), 2, "{planned:?}");
    assert_eq!(pick(&report.rules, "agent-ok").over_cap.len(), 1);

    let tracker = Fake::default();
    let worker = crate::sync::Ingest::new(
        Ado::new(
            "contoso",
            "https://dev.azure.com/",
            ado_org(items, None),
            vec!["KBL".into()],
            crate::ado_scopes(&cfg),
            &cfg.ado.as_ref().unwrap().projects,
            &cfg.ado.as_ref().unwrap().pr_url_base,
        ),
        tracker.clone(),
        crate::sync::Settings {
            rules: cfg.rules.clone(),
            repos: cfg.repos(),
            reconcile_period: None,
            release_compares: 20,
        },
    );
    worker.poll(false).await.unwrap();
    assert_eq!(imported(&tracker).await, planned);
}

fn gitlab_config(rule: Value) -> Config {
    let mut base = json!({ "name": "agent-ok", "repo": "team/app",
        "labels_all": ["agent-ok"], "mode": "admit" });
    base.as_object_mut()
        .unwrap()
        .extend(rule.as_object().unwrap().clone());
    Config::parse(
        &json!({
            "tracker": { "url": "http://t", "token_env": "T" },
            "gitlab": { "api": "https://gitlab.example.com", "token_env": "GL",
                        "projects": { "team/app": "group/sub/app" } },
            "rules": [base],
        })
        .to_string(),
    )
    .unwrap()
}

fn gitlab_issue(iid: u64, labels: &[&str]) -> Value {
    json!({
        "id": 9000 + iid, "iid": iid, "title": format!("Issue {iid}"),
        "description": "text", "state": "opened", "labels": labels,
        "assignees": [], "author": { "username": "alice" }, "type": "ISSUE",
        "web_url": format!("https://gitlab.example.com/group/sub/app/-/issues/{iid}"),
        "user_notes_count": 0, "moved_to_id": null,
        "updated_at": format!("2026-11-10T12:00:0{iid}.000Z"),
    })
}

/// A GitLab instance. `token` is what the token's own details say.
fn lab(token: Res, access: u64, issues: Vec<Value>) -> Vendor {
    Vendor::new(move |req| {
        let p = path(req);
        if p == "/api/v4/user" {
            return res(200, json!({ "username": "kbl-bot" }));
        }
        if p == "/api/v4/personal_access_tokens/self" {
            return token.clone();
        }
        if p == "/api/v4/projects/group%2Fsub%2Fapp" {
            return res(
                200,
                json!({ "permissions": { "project_access": { "access_level": access } } }),
            );
        }
        if p == "/api/v4/projects/group%2Fsub%2Fapp/issues" {
            return res(200, Value::Array(issues.clone()));
        }
        if p.ends_with("/notes") {
            return res(200, json!([]));
        }
        res(404, Value::Null)
    })
}

fn token(scopes: &[&str], expires: Option<&str>) -> Res {
    res(
        200,
        json!({ "scopes": scopes, "expires_at": expires, "active": true, "revoked": false }),
    )
}

fn gitlab_source(v: &Vendor, cfg: &Config) -> Gitlab<ReadOnly<Vendor>> {
    let g = cfg.gitlab.as_ref().unwrap();
    Gitlab::new(
        &g.api,
        ReadOnly(v.clone()),
        vec![],
        cfg.rules.clone(),
        &g.projects,
    )
    .unwrap()
}

#[tokio::test]
async fn gitlab_sign_in_scopes_expiry_and_role_are_reported() {
    let v = lab(token(&["api"], Some("2099-01-01")), 30, vec![]);
    let cfg = gitlab_config(json!({ "comment": true }));
    let mut out = Vec::new();
    gitlab_findings(&cfg, &mut gitlab_source(&v, &cfg), &mut out).await;
    assert!(has(&out, Level::Ok, "signs in as kbl-bot"), "{out:?}");
    assert!(has(&out, Level::Ok, "token scopes: api"));
    assert!(has(&out, Level::Ok, "expires on 2099-01-01"));
    assert!(has(&out, Level::Ok, "can read group/sub/app"));
    assert!(has(&out, Level::Ok, "role in group/sub/app is enough"));
    assert!(messages(&out, Level::Error).is_empty(), "{out:?}");
}

#[tokio::test]
async fn a_rejected_gitlab_token_is_one_error() {
    let v = Vendor::new(|_| res(401, Value::Null));
    let cfg = gitlab_config(json!({}));
    let mut out = Vec::new();
    gitlab_findings(&cfg, &mut gitlab_source(&v, &cfg), &mut out).await;
    assert!(
        has(&out, Level::Error, "rejected the token (401)"),
        "{out:?}"
    );
    assert_eq!(out.len(), 1);
    assert!(!v.touched("/projects/"));
}

#[tokio::test]
async fn a_gitlab_token_without_api_cannot_write_back() {
    let cfg = gitlab_config(json!({ "comment": true }));
    let v = lab(token(&["read_api"], None), 30, vec![]);
    let mut out = Vec::new();
    gitlab_findings(&cfg, &mut gitlab_source(&v, &cfg), &mut out).await;
    assert!(has(&out, Level::Error, "lacks the api scope"), "{out:?}");

    // A reading rule is fine with read_api alone.
    let cfg = gitlab_config(json!({}));
    let mut out = Vec::new();
    gitlab_findings(&cfg, &mut gitlab_source(&v, &cfg), &mut out).await;
    assert!(messages(&out, Level::Error).is_empty(), "{out:?}");

    // A token with neither scope cannot read.
    let v = lab(token(&["read_user"], None), 30, vec![]);
    let mut out = Vec::new();
    gitlab_findings(&cfg, &mut gitlab_source(&v, &cfg), &mut out).await;
    assert!(has(&out, Level::Error, "needs read_api"), "{out:?}");
}

#[tokio::test]
async fn a_gitlab_guest_may_comment_but_not_close() {
    let v = lab(token(&["api"], None), 10, vec![]);
    let cfg = gitlab_config(json!({ "comment": true }));
    let mut out = Vec::new();
    gitlab_findings(&cfg, &mut gitlab_source(&v, &cfg), &mut out).await;
    assert!(messages(&out, Level::Error).is_empty(), "{out:?}");

    let cfg = gitlab_config(json!({ "transition_on_merge": "close" }));
    let mut out = Vec::new();
    gitlab_findings(&cfg, &mut gitlab_source(&v, &cfg), &mut out).await;
    assert!(has(&out, Level::Error, "below Reporter"), "{out:?}");
}

#[tokio::test]
async fn a_gitlab_project_that_lists_no_role_is_a_warning_not_an_error() {
    let v = Vendor::new(|req| match path(req).as_str() {
        "/api/v4/user" => res(200, json!({ "username": "kbl-bot" })),
        "/api/v4/personal_access_tokens/self" => token(&["api"], None),
        "/api/v4/projects/group%2Fsub%2Fapp" => res(
            200,
            json!({ "permissions": {
            "project_access": null, "group_access": null } }),
        ),
        _ => res(404, Value::Null),
    });
    let cfg = gitlab_config(json!({ "comment": true }));
    let mut out = Vec::new();
    gitlab_findings(&cfg, &mut gitlab_source(&v, &cfg), &mut out).await;
    assert!(messages(&out, Level::Error).is_empty(), "{out:?}");
    assert!(
        has(&out, Level::Warn, "could not tell the token's role"),
        "{out:?}"
    );
}

#[tokio::test]
async fn a_revoked_or_nearly_expired_gitlab_token_is_flagged() {
    let cfg = gitlab_config(json!({}));
    let revoked = res(
        200,
        json!({ "scopes": ["api"], "expires_at": null, "active": false, "revoked": true }),
    );
    let mut out = Vec::new();
    gitlab_findings(
        &cfg,
        &mut gitlab_source(&lab(revoked, 30, vec![]), &cfg),
        &mut out,
    )
    .await;
    assert!(
        has(&out, Level::Error, "revoked or no longer active"),
        "{out:?}"
    );

    let soon = std::time::SystemTime::now() + std::time::Duration::from_secs(3 * 86_400);
    let secs = soon
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let date = civil(secs / 86_400);
    let v = lab(token(&["api"], Some(&date)), 30, vec![]);
    let mut out = Vec::new();
    gitlab_findings(&cfg, &mut gitlab_source(&v, &cfg), &mut out).await;
    assert!(
        has(&out, Level::Warn, &format!("expires on {date}")),
        "{out:?}"
    );
}

/// YYYY-MM-DD of a day count since the epoch.
fn civil(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[test]
fn the_sources_check_builds_are_guarded_by_type() {
    let cfg = ado_config(json!({}));
    let budget = Budget::new(crate::budget::Vendor::Ado, None, 0.5);
    let _: Ado<ReadOnlyWiql<Budgeted<crate::ado::transport::Reqwest>>> =
        ado_guarded(&cfg, cfg.ado.as_ref().unwrap(), &budget).unwrap();
    let cfg = gitlab_config(json!({}));
    let _: Gitlab<ReadOnly<Budgeted<crate::gitlab::transport::Reqwest>>> =
        gitlab_guarded(&cfg, cfg.gitlab.as_ref().unwrap(), &budget).unwrap();
}

#[tokio::test]
async fn an_expired_or_unreported_gitlab_token_is_said_plainly() {
    let cfg = gitlab_config(json!({}));
    let v = lab(token(&["api"], Some("2020-01-01")), 30, vec![]);
    let mut out = Vec::new();
    gitlab_findings(&cfg, &mut gitlab_source(&v, &cfg), &mut out).await;
    assert!(has(&out, Level::Error, "expired on 2020-01-01"), "{out:?}");

    // GitLab before 15.5 has no endpoint for it: a warning, and reads go on.
    let v = lab(res(404, Value::Null), 30, vec![]);
    let mut out = Vec::new();
    gitlab_findings(&cfg, &mut gitlab_source(&v, &cfg), &mut out).await;
    assert!(
        has(&out, Level::Warn, "scopes and expiry are not checked"),
        "{out:?}"
    );
    assert!(has(&out, Level::Ok, "can read group/sub/app"));
    assert!(messages(&out, Level::Error).is_empty());
}

#[tokio::test]
async fn the_gitlab_check_sends_no_write_and_the_dry_run_matches_the_worker() {
    let issues = vec![
        gitlab_issue(1, &["agent-ok"]),
        gitlab_issue(2, &["agent-ok"]),
        gitlab_issue(3, &["other"]),
        gitlab_issue(4, &["agent-ok"]),
    ];
    let cfg = gitlab_config(json!({ "cap": 2, "comment": true }));
    let v = lab(token(&["api"], None), 30, issues.clone());
    let mut report = Report::default();
    let mut gitlab = gitlab_source(&v, &cfg);
    gitlab_findings(&cfg, &mut gitlab, &mut report.findings).await;
    dry_run(&cfg, &gitlab, None::<&Fake>, &mut report).await;
    assert!(v.non_gets().is_empty(), "{:?}", v.non_gets());
    assert!(v.touched("/issues"));
    let planned = ids(&report, "agent-ok");
    assert_eq!(planned.len(), 2, "{planned:?}");

    let tracker = Fake::default();
    let g = cfg.gitlab.as_ref().unwrap();
    let worker = crate::sync::Ingest::new(
        Gitlab::new(
            &g.api,
            lab(token(&["api"], None), 30, issues),
            vec!["kbl-bot".into()],
            cfg.rules.clone(),
            &g.projects,
        )
        .unwrap(),
        tracker.clone(),
        crate::sync::Settings {
            rules: cfg.rules.clone(),
            repos: cfg.repos(),
            reconcile_period: None,
            release_compares: 20,
        },
    );
    worker.poll(false).await.unwrap();
    assert_eq!(imported(&tracker).await, planned);
}

#[tokio::test]
async fn an_ado_or_gitlab_config_gets_real_findings_and_no_secret() {
    // Loopback addresses, so a request fails at once; the credentials are unset.
    let cases = [
        (
            "ado",
            r#"{"tracker":{"url":"http://127.0.0.1:1","token_env":"KBL_CHECK_NO_TOKEN"},"ado":{"organization":"contoso","api":"http://127.0.0.1:1","pat_env":"KBL_CHECK_NO_ADO_PAT"},"rules":[{"name":"r","repo":"contoso/Platform","mode":"admit"}]}"#,
        ),
        (
            "gitlab",
            r#"{"tracker":{"url":"http://127.0.0.1:1","token_env":"KBL_CHECK_NO_TOKEN"},"gitlab":{"api":"http://127.0.0.1:1","token_env":"KBL_CHECK_NO_GL"},"rules":[{"name":"r","repo":"team/app","mode":"admit"}]}"#,
        ),
    ];
    for (source, cfg) in cases {
        let path = temp(&format!("real-{source}.json"), cfg);
        let report = report_for(&path).await;
        assert!(!report.ok);
        let text = render(&report);
        assert!(!text.contains("does not test"), "{text}");
        assert!(text.contains(&format!("ERROR {source}:")), "{text}");
        assert!(
            text.contains(&format!("parses: 1 rule(s) for {source}")),
            "{text}"
        );
    }
}

/// A transport whose failure text quotes a token, as a library error can.
struct Leaky;

impl Transport for Leaky {
    async fn send(&self, _req: Req) -> Result<Res> {
        Err(anyhow!("connect failed with glpat-abcdefghij0123456789"))
    }
}

#[tokio::test]
async fn a_transport_error_that_quotes_a_token_is_cut_from_the_report() {
    let cfg = gitlab_config(json!({}));
    let g = cfg.gitlab.as_ref().unwrap();
    let mut gitlab = Gitlab::new(
        &g.api,
        ReadOnly(Leaky),
        vec![],
        cfg.rules.clone(),
        &g.projects,
    )
    .unwrap();
    let mut out = Vec::new();
    gitlab_findings(&cfg, &mut gitlab, &mut out).await;
    assert!(has(&out, Level::Error, "connect failed"), "{out:?}");
    assert!(
        !format!("{out:?}").contains("abcdefghij0123456789"),
        "{out:?}"
    );

    let cfg = ado_config(json!({}));
    let a = cfg.ado.as_ref().unwrap();
    let mut ado = Ado::new(
        &a.organization,
        &a.api,
        ReadOnlyWiql(Leaky),
        vec![],
        crate::ado_scopes(&cfg),
        &a.projects,
        &a.pr_url_base,
    );
    let mut out = Vec::new();
    ado_findings(&cfg, &mut ado, &mut out).await;
    assert!(
        !format!("{out:?}").contains("abcdefghij0123456789"),
        "{out:?}"
    );
}

#[tokio::test]
async fn check_reports_the_vendors_remaining_budget_and_the_projection() {
    let gh = Script::default();
    let reset = (super::epoch_secs() + 1800).to_string();
    gh.on(
        "GET",
        "/user",
        Res {
            body: json!({ "login": "kbl[bot]" }),
            ..status(
                200,
                &[
                    ("x-ratelimit-limit", "5000"),
                    ("x-ratelimit-remaining", "4321"),
                    ("x-ratelimit-reset", &reset),
                ],
            )
        },
    );
    let budget = Budget::new(crate::budget::Vendor::Github, None, 0.5);
    let source = Github::new(API, ReadOnly(budgeted(gh, budget.clone())), vec![]);
    source.login().await;
    let cfg = config(vec![]);
    let f = budget_finding(&cfg, &source, None::<&Fake>, &budget).await;
    assert_eq!(f.scope, "budget");
    assert_eq!(f.level, Level::Ok);
    for want in [
        "4321 of 5000 requests left",
        "resetting in 30 min",
        "may spend 2500 an hour",
        "projected to spend about 24 an hour",
    ] {
        assert!(f.message.contains(want), "{want}: {}", f.message);
    }
}

#[tokio::test]
async fn check_warns_when_the_config_cannot_fit_its_budget() {
    let gh = Script::default();
    let mut cfg = config(vec![]);
    cfg.budget_per_hour = Some(10);
    let budget = Budget::new(crate::budget::Vendor::Github, Some(10), 0.5);
    let source = Github::new(API, ReadOnly(budgeted(gh, budget.clone())), vec![]);
    let f = budget_finding(&cfg, &source, None::<&Fake>, &budget).await;
    assert_eq!(f.level, Level::Warn, "{}", f.message);
    assert!(f.message.contains("stretch"), "{}", f.message);
}
