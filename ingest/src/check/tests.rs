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
    let v: Value = serde_json::to_value(&report).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["rules"][0]["would_import"][0]["issue"], "o/n#1");
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

#[tokio::test]
async fn an_ado_or_gitlab_config_is_reported_not_a_panic() {
    let cases = [
        (
            "ado",
            r#"{"tracker":{"url":"http://127.0.0.1:1","token_env":"KBL_CHECK_NO_TOKEN"},"ado":{"organization":"contoso","pat_env":"P"},"rules":[{"name":"r","repo":"contoso/Platform","mode":"admit"}]}"#,
        ),
        (
            "gitlab",
            r#"{"tracker":{"url":"http://127.0.0.1:1","token_env":"KBL_CHECK_NO_TOKEN"},"gitlab":{"token_env":"G"},"rules":[{"name":"r","repo":"team/app","mode":"admit"}]}"#,
        ),
    ];
    for (source, cfg) in cases {
        let path = temp(&format!("unsupported-{source}.json"), cfg);
        let report = report_for(&path).await;
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.scope == source && f.message.contains("does not test")),
            "{:?}",
            report.findings
        );
    }
}
