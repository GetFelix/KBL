//! `kbl-ingest check`: what an operator needs to know before starting the
//! worker. It parses the config with the worker's own parser, tests each
//! credential against its vendor, reports the permissions every rule needs,
//! and lists what each rule would import on a first poll.
//!
//! It reads only. The vendor transport refuses any request that is not a GET
//! and the tracker is called through `roots` and `load`, so a check cannot
//! change either side. An App credential still mints its installation token,
//! which is how it is tested. Output names environment variables and files,
//! never their contents.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde_json::Value;

use crate::config::{Config, GithubConfig, Mode, Rule};
use crate::github::transport::Reqwest as GithubHttp;
use crate::github::{Github, Req, Res, Transport};
use crate::jira::Jira;
use crate::jira::transport::Reqwest as JiraHttp;
use crate::rules::first_match;
use crate::source::{Cursor, Source};
use crate::state::State;
use crate::tracker_api::{Http, Tracker};

/// Issues listed per rule in the readable output; `--json` has them all.
const SHOWN: usize = 10;

#[derive(clap::Args)]
pub(crate) struct Args {
    /// The config to check, the one `kbl-ingest --config` takes.
    #[arg(long, env = "KBL_INGEST_CONFIG")]
    config: PathBuf,
    /// Print one JSON report instead of text.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Level {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Finding {
    pub(crate) level: Level,
    /// What was checked: `config`, `tracker`, `github`, `repo owner/name`, `rule name`.
    pub(crate) scope: String,
    pub(crate) message: String,
}

impl Finding {
    fn new(level: Level, scope: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            level,
            scope: scope.into(),
            message: message.into(),
        }
    }
}

/// An issue a rule would take, or hold back.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Pick {
    pub(crate) issue: String,
    pub(crate) title: String,
    pub(crate) tier: u8,
    /// None when no repository is mapped: it would go to Needs you.
    pub(crate) target: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RuleReport {
    pub(crate) rule: String,
    pub(crate) repo: String,
    pub(crate) mode: &'static str,
    pub(crate) cap: Option<u32>,
    /// Open roots the rule has now, from the tracker.
    pub(crate) open_now: usize,
    pub(crate) would_import: Vec<Pick>,
    /// Matches left waiting upstream because the rule is at its cap.
    pub(crate) over_cap: Vec<Pick>,
    /// Matches the tracker already has a root for.
    pub(crate) already_imported: usize,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Report {
    pub(crate) ok: bool,
    pub(crate) findings: Vec<Finding>,
    pub(crate) rules: Vec<RuleReport>,
    /// Open issues listed that no rule takes.
    pub(crate) unmatched: usize,
}

impl Report {
    fn finish(mut self) -> Self {
        self.ok = !self.findings.iter().any(|f| f.level == Level::Error);
        self
    }
}

/// Wraps a vendor transport so no request but a GET can leave the process.
pub(crate) struct ReadOnly<T>(pub(crate) T);

impl<T: Transport> Transport for ReadOnly<T> {
    async fn send(&self, req: Req) -> Result<Res> {
        if req.method != "GET" {
            return Err(anyhow!(
                "check does not write: refused a {} request",
                req.method
            ));
        }
        self.0.send(req).await
    }
}

/// Runs the check and prints it. False when anything is an error.
pub(crate) async fn run(args: &Args) -> Result<bool> {
    let report = report_for(&args.config).await;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render(&report));
    }
    Ok(report.ok)
}

async fn report_for(path: &Path) -> Report {
    let cfg = match Config::load(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            return Report {
                findings: vec![Finding::new(Level::Error, "config", safe(format!("{e:#}")))],
                ..Report::default()
            }
            .finish();
        }
    };
    let mut report = Report::default();
    report.findings.push(Finding::new(
        Level::Ok,
        "config",
        format!(
            "{} parses: {} rule(s) for {}",
            path.display(),
            cfg.rules.len(),
            if cfg.github.is_some() {
                "github"
            } else {
                "jira"
            }
        ),
    ));
    report
        .findings
        .extend(static_findings(&cfg, &|n| std::env::var(n).ok()));

    let tracker = match std::env::var(&cfg.tracker.token_env) {
        Ok(token) if !token.is_empty() => Http::new(&cfg.tracker.url, token).ok(),
        _ => None,
    };
    match (&cfg.github, &cfg.jira) {
        (Some(g), None) => match crate::github_token(g).and_then(GithubHttp::new) {
            Ok(http) => {
                let github = Github::new(&g.api, ReadOnly(http), g.own_logins.clone());
                github_findings(&cfg, g, &github, &mut report.findings).await;
                dry_run(&cfg, &github, tracker.as_ref(), &mut report).await;
            }
            Err(e) => {
                report
                    .findings
                    .push(Finding::new(Level::Error, "github", safe(format!("{e:#}"))))
            }
        },
        (None, Some(j)) => match JiraHttp::new(crate::jira_auth(j)) {
            Ok(http) => {
                let mut jira = Jira::new(
                    &j.site,
                    &j.api,
                    ReadOnly(http),
                    j.own_logins.clone(),
                    crate::jira_scopes(&cfg),
                    &j.pr_url_base,
                );
                jira_findings(&cfg, &mut jira, &mut report.findings).await;
                dry_run(&cfg, &jira, tracker.as_ref(), &mut report).await;
            }
            Err(e) => {
                report
                    .findings
                    .push(Finding::new(Level::Error, "jira", safe(format!("{e:#}"))))
            }
        },
        _ => unreachable!("the config checks for exactly one source"),
    }
    if let Some(t) = &tracker {
        report.findings.push(tracker_finding(&cfg, t).await);
    } else {
        report.findings.push(Finding::new(
            Level::Error,
            "tracker",
            "not reached: no usable token (see above); the dry run ignores what is already imported",
        ));
    }
    report.finish()
}

async fn tracker_finding<T: Tracker>(cfg: &Config, tracker: &T) -> Finding {
    match tracker.roots().await {
        Ok(listed) => Finding::new(
            Level::Ok,
            "tracker",
            format!(
                "{} answers the ingest token ({} imported root(s))",
                cfg.tracker.url,
                listed.roots.len()
            ),
        ),
        Err(e) => Finding::new(
            Level::Error,
            "tracker",
            safe(format!(
                "{} did not accept the ingest token: {e:#}",
                cfg.tracker.url
            )),
        ),
    }
}

/// Error text with any known secret shape cut out.
fn safe(text: impl Display) -> String {
    kbl_core::redact::redact(&text.to_string()).into_owned()
}

/// Checks that need no network: the files and variables the credentials come
/// from, and rules that will do something surprising. `env` reads a variable.
pub(crate) fn static_findings(cfg: &Config, env: &dyn Fn(&str) -> Option<String>) -> Vec<Finding> {
    let mut out = Vec::new();
    let var = |scope: &str, name: &str| {
        if env(name).is_some_and(|v| !v.trim().is_empty()) {
            Finding::new(Level::Ok, scope, format!("{name} is set"))
        } else {
            Finding::new(
                Level::Error,
                scope,
                format!("{name} is not set or is empty"),
            )
        }
    };
    out.push(var("tracker", &cfg.tracker.token_env));
    if let Some(g) = &cfg.github {
        if let Some(name) = &g.token_env {
            out.push(var("github", name));
        }
        if !g.api.starts_with("https://") {
            out.push(Finding::new(
                Level::Warn,
                "github",
                format!("api {} is not https", g.api),
            ));
        }
        let set: Vec<&str> = [
            ("token_file", g.token_file.is_some()),
            ("app", g.app.is_some()),
            ("token_env", g.token_env.is_some()),
        ]
        .iter()
        .filter(|s| s.1)
        .map(|s| s.0)
        .collect();
        if set.len() > 1 {
            out.push(Finding::new(
                Level::Warn,
                "github",
                format!(
                    "several credentials are set ({}); {} is used and the rest are ignored",
                    set.join(", "),
                    set[0]
                ),
            ));
        }
        if let Some(f) = &g.token_file {
            out.push(secret_file("github", "token_file", f));
        }
        if let Some(app) = &g.app {
            out.push(
                match crate::github::app::check_key_mode(&app.private_key_file) {
                    Ok(()) => secret_file("github", "app.private_key_file", &app.private_key_file),
                    Err(e) => Finding::new(Level::Error, "github", format!("{e:#}")),
                },
            );
        }
    }
    if let Some(j) = &cfg.jira {
        if let Some(name) = &j.token_env {
            out.push(var("jira", name));
        }
        if let Some(f) = &j.token_file {
            out.push(secret_file("jira", "token_file", f));
        }
        if let Some(f) = &j.oauth_token_file {
            out.push(secret_file("jira", "oauth_token_file", f));
            if j.browse_url.is_none() {
                out.push(Finding::new(
                    Level::Warn,
                    "jira",
                    "oauth_token_file is set without browse_url, so issue links are built from the api address, which people cannot open",
                ));
            }
        }
    }
    for r in &cfg.rules {
        let scope = format!("rule {}", r.name);
        if r.cap == Some(0) {
            out.push(Finding::new(
                Level::Warn,
                &scope,
                "cap is 0: nothing will be imported",
            ));
        }
        if cfg.github.is_some() && is_open_filter(r) {
            out.push(Finding::new(
                Level::Warn,
                &scope,
                format!(
                    "no label, milestone, assignee or type filter: it takes every open issue in {}",
                    r.repo
                ),
            ));
        }
        if r.transition_on_merge.is_some() {
            out.push(Finding::new(
                Level::Warn,
                &scope,
                "transition_on_merge fires only when a merged pr_status comes from elsewhere; a Jira worker records no merges",
            ));
        }
    }
    out
}

fn is_open_filter(r: &Rule) -> bool {
    r.labels_all.is_empty()
        && r.labels_any.is_empty()
        && r.milestone.is_none()
        && r.assignee.is_none()
        && r.issue_type.is_none()
}

/// A credential file: present, readable and not empty. Its contents are never shown.
fn secret_file(scope: &str, key: &str, path: &Path) -> Finding {
    match std::fs::read_to_string(path) {
        Ok(t) if t.trim().is_empty() => Finding::new(
            Level::Error,
            scope,
            format!("{key} {} is empty", path.display()),
        ),
        Ok(_) => Finding::new(
            Level::Ok,
            scope,
            format!("{key} {} is readable", path.display()),
        ),
        Err(e) => Finding::new(
            Level::Error,
            scope,
            format!("{key} {} cannot be read: {}", path.display(), e.kind()),
        ),
    }
}

/// Which repository rights a GitHub rule needs, and what the credential has.
pub(crate) async fn github_findings<T: Transport>(
    cfg: &Config,
    gh: &GithubConfig,
    github: &Github<T>,
    out: &mut Vec<Finding>,
) {
    match github.probe("/user").await {
        Ok(res) if res.status == 200 => {
            let who = res.body["login"].as_str().unwrap_or("a user");
            out.push(Finding::new(
                Level::Ok,
                "github",
                format!("the credential signs in as {who}"),
            ));
        }
        Ok(res) if res.status == 401 => out.push(Finding::new(
            Level::Error,
            "github",
            "GitHub rejected the credential (401): it is wrong, expired or revoked",
        )),
        // An App installation token has no user behind it.
        Ok(res) if matches!(res.status, 403 | 404) => {
            out.push(Finding::new(
                Level::Ok,
                "github",
                "the credential is accepted; it has no user, as an App installation token has none",
            ));
            if gh.own_logins.is_empty() {
                out.push(Finding::new(
                    Level::Warn,
                    "github",
                    "own_logins is empty: list the App's bot login, such as kbl[bot], or ingest imports its own comments back",
                ));
            }
        }
        Ok(res) => out.push(Finding::new(
            Level::Error,
            "github",
            format!("GitHub answered {} to the sign-in test", res.status),
        )),
        Err(e) => {
            out.push(Finding::new(Level::Error, "github", safe(e)));
            return;
        }
    }

    let mut seen: BTreeMap<&str, Option<Value>> = BTreeMap::new();
    for r in &cfg.rules {
        let scope = format!("repo {}", r.repo);
        if !seen.contains_key(r.repo.as_str()) {
            let permissions = match github.probe(&format!("/repos/{}", r.repo)).await {
                Ok(res) if res.status == 200 => {
                    out.push(Finding::new(
                        Level::Ok,
                        &scope,
                        "the credential can read it",
                    ));
                    Some(res.body["permissions"].clone())
                }
                Ok(res) if res.status == 404 => {
                    out.push(Finding::new(
                        Level::Error,
                        &scope,
                        "not found, or the credential cannot see it (GitHub answers 404 for both); \
                         grant it read access to issues and pull requests",
                    ));
                    None
                }
                Ok(res) => {
                    out.push(Finding::new(
                        Level::Error,
                        &scope,
                        format!("GitHub answered {} reading it", res.status),
                    ));
                    None
                }
                Err(e) => {
                    out.push(Finding::new(Level::Error, &scope, safe(e)));
                    None
                }
            };
            seen.insert(&r.repo, permissions);
        }
        let Some(Some(perms)) = seen.get(r.repo.as_str()) else {
            continue;
        };
        let has = |k: &str| perms[k].as_bool();
        let rule_scope = format!("rule {}", r.name);
        // GitHub omits the summary for some credentials; then nothing is known.
        let known = perms.is_object();
        let write = ["push", "maintain", "admin"]
            .iter()
            .any(|k| has(k) == Some(true));
        let triage = write || has("triage") == Some(true);
        if r.open_prerequisites {
            out.push(match (known, triage) {
                (true, true) => Finding::new(Level::Ok, &rule_scope, format!("issues: write on {}", r.repo)),
                (true, false) => Finding::new(
                    Level::Error,
                    &rule_scope,
                    format!("open_prerequisites needs issues: write on {}, which the credential lacks", r.repo),
                ),
                (false, _) => Finding::new(
                    Level::Warn,
                    &rule_scope,
                    format!("open_prerequisites needs issues: write on {}; GitHub did not say what the credential may do", r.repo),
                ),
            });
        }
        out.push(match (known, write) {
            (true, true) => Finding::new(Level::Ok, &rule_scope, format!("pull requests: write on {}", r.repo)),
            (true, false) => Finding::new(
                Level::Warn,
                &rule_scope,
                format!("no pull requests: write on {}: when an issue is dropped upstream, ingest cannot make the agent's open pull request a draft", r.repo),
            ),
            (false, _) => Finding::new(
                Level::Warn,
                &rule_scope,
                format!("pull requests: write on {} could not be determined; it is needed only to draft a pull request after its issue is dropped", r.repo),
            ),
        });
    }
}

/// The Jira permission each rule option needs.
pub(crate) async fn jira_findings<T: Transport>(
    cfg: &Config,
    jira: &mut Jira<T>,
    out: &mut Vec<Finding>,
) {
    match jira.probe("/myself", &[]).await {
        Ok(res) if res.status == 200 => {
            let names: Vec<String> = ["displayName", "accountId"]
                .iter()
                .filter_map(|k| res.body[k].as_str().map(str::to_owned))
                .collect();
            out.push(Finding::new(
                Level::Ok,
                "jira",
                format!(
                    "the credential signs in as {}",
                    names.first().map_or("an account", String::as_str)
                ),
            ));
            jira.add_own(names);
        }
        Ok(res) if matches!(res.status, 401 | 403) => out.push(Finding::new(
            Level::Error,
            "jira",
            format!(
                "Jira refused the credential ({}): check the email, the token and, for OAuth, the read:me scope",
                res.status
            ),
        )),
        Ok(res) => out.push(Finding::new(
            Level::Error,
            "jira",
            format!("Jira answered {} to the sign-in test", res.status),
        )),
        Err(e) => {
            out.push(Finding::new(Level::Error, "jira", safe(e)));
            return;
        }
    }
    if let Err(e) = jira.require_own() {
        out.push(Finding::new(Level::Error, "jira", format!("{e:#}")));
    }
    let mut projects = BTreeSet::new();
    for r in &cfg.rules {
        let Some(project) = r.repo.split_once('/').map(|p| p.1) else {
            continue;
        };
        let wanted = [
            ("BROWSE_PROJECTS", true, "browse the project"),
            ("ADD_COMMENTS", r.comment, "add comments (comment)"),
            ("LINK_ISSUES", r.link_pr, "link issues (link_pr)"),
            (
                "TRANSITION_ISSUES",
                r.transition_on_pr.is_some() || r.transition_on_merge.is_some(),
                "transition issues (transition_on_pr, transition_on_merge)",
            ),
        ];
        let keys: Vec<&str> = wanted.iter().map(|w| w.0).collect();
        let scope = format!("rule {}", r.name);
        let res = jira
            .probe(
                "/mypermissions",
                &[("projectKey", project), ("permissions", &keys.join(","))],
            )
            .await;
        match res {
            Ok(res) if res.status == 200 => {
                for (key, needed, what) in wanted {
                    if !needed {
                        continue;
                    }
                    if res.body["permissions"][key]["havePermission"] == Value::Bool(true) {
                        out.push(Finding::new(
                            Level::Ok,
                            &scope,
                            format!("may {what} in {project}"),
                        ));
                    } else {
                        out.push(Finding::new(
                            Level::Error,
                            &scope,
                            format!("the credential may not {what} in {project}"),
                        ));
                    }
                }
            }
            Ok(res) => {
                if projects.insert(project) {
                    out.push(Finding::new(
                        Level::Error,
                        &scope,
                        format!("Jira answered {} for the permissions in {project}: the project does not exist or is not visible", res.status),
                    ));
                }
            }
            Err(e) => out.push(Finding::new(Level::Error, &scope, safe(e))),
        }
    }
}

/// Lists what each rule would import on a first poll, as the worker decides it:
/// open issues in the rules' repos, the first matching rule, each rule's cap
/// against the roots it already has. Nothing is written.
pub(crate) async fn dry_run<S: Source, T: Tracker>(
    cfg: &Config,
    source: &S,
    tracker: Option<&T>,
    report: &mut Report,
) {
    let changes = match source
        .changed(&cfg.repos(), &BTreeMap::<String, Cursor>::new())
        .await
    {
        Ok(c) => c,
        Err(e) => {
            report.findings.push(Finding::new(
                Level::Error,
                "dry run",
                format!("listing the issues failed: {}", safe(e)),
            ));
            return;
        }
    };
    for (repo, code) in &changes.denied {
        report.findings.push(Finding::new(
            Level::Error,
            format!("repo {repo}"),
            format!(
                "the source answered {code} listing its issues; for GitHub the credential cannot read them, \
                 for Jira a 400 is a JQL it rejects and a 401 or 403 is no access"
            ),
        ));
    }

    let mut imported: BTreeSet<String> = BTreeSet::new();
    let mut open: BTreeMap<String, usize> = BTreeMap::new();
    if let Some(t) = tracker
        && let Ok(listed) = t.roots().await
    {
        {
            let roots: BTreeMap<_, _> = listed
                .roots
                .iter()
                .map(|r| (r.upstream_id.clone(), r.closed))
                .collect();
            imported = roots.keys().cloned().collect();
            let state = t
                .load(source.name())
                .await
                .ok()
                .and_then(|(v, _)| v)
                .and_then(|v| serde_json::from_value::<State>(v).ok())
                .unwrap_or_default();
            // The worker's own count: items a rule admitted whose root is still open.
            for (key, tracked) in &state.items {
                if tracked.moved_to.is_none() && roots.get(key) == Some(&false) {
                    *open.entry(tracked.rule.clone()).or_default() += 1;
                }
            }
        }
    }

    let mut rules: Vec<RuleReport> = cfg
        .rules
        .iter()
        .map(|r| RuleReport {
            rule: r.name.clone(),
            repo: r.repo.clone(),
            mode: match r.mode {
                Mode::Admit => "admit",
                Mode::Propose => "propose",
            },
            cap: r.cap,
            open_now: open.get(&r.name).copied().unwrap_or(0),
            would_import: Vec::new(),
            over_cap: Vec::new(),
            already_imported: 0,
        })
        .collect();
    for issue in changes.issues.iter().filter(|i| i.closed.is_none()) {
        let Some(rule) = first_match(&cfg.rules, issue) else {
            report.unmatched += 1;
            continue;
        };
        let slot = cfg
            .rules
            .iter()
            .position(|r| r.name == rule.name)
            .expect("a rule of the config");
        let out = &mut rules[slot];
        if imported.contains(&issue.id.to_string()) {
            out.already_imported += 1;
            continue;
        }
        let pick = Pick {
            issue: issue.id.to_string(),
            title: issue.title.chars().take(100).collect(),
            tier: rule.tier_for(issue),
            target: rule.target_for(issue),
        };
        let full = rule
            .cap
            .is_some_and(|c| out.open_now + out.would_import.len() >= c as usize);
        if full {
            out.over_cap.push(pick);
        } else {
            out.would_import.push(pick);
        }
    }
    report.rules = rules;
}

fn render(r: &Report) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for f in &r.findings {
        let tag = match f.level {
            Level::Ok => "ok   ",
            Level::Warn => "warn ",
            Level::Error => "ERROR",
        };
        let _ = writeln!(s, "{tag} {}: {}", f.scope, f.message);
    }
    if !r.rules.is_empty() {
        let _ = writeln!(
            s,
            "\nDry run (nothing is written): what the first poll would do"
        );
    }
    for rule in &r.rules {
        let cap = rule.cap.map_or("no cap".into(), |c| {
            format!("cap {c}, {} open now", rule.open_now)
        });
        let _ = writeln!(
            s,
            "\nrule {} ({}, {}, {cap}): {} to import, {} over the cap, {} already imported",
            rule.rule,
            rule.repo,
            rule.mode,
            rule.would_import.len(),
            rule.over_cap.len(),
            rule.already_imported
        );
        for p in rule.would_import.iter().take(SHOWN) {
            let _ = writeln!(
                s,
                "  import {} (tier {}, work in {}) {}",
                p.issue,
                p.tier,
                p.target.as_deref().unwrap_or("no repository: Needs you"),
                p.title
            );
        }
        if rule.would_import.len() > SHOWN {
            let _ = writeln!(
                s,
                "  ... and {} more (--json lists all)",
                rule.would_import.len() - SHOWN
            );
        }
        for p in rule.over_cap.iter().take(SHOWN) {
            let _ = writeln!(s, "  waits {} {}", p.issue, p.title);
        }
    }
    if !r.rules.is_empty() {
        let _ = writeln!(s, "\n{} open issue(s) match no rule.", r.unmatched);
    }
    let errors = r
        .findings
        .iter()
        .filter(|f| f.level == Level::Error)
        .count();
    let _ = writeln!(
        s,
        "\n{}",
        if errors == 0 {
            "No errors.".to_owned()
        } else {
            format!("{errors} error(s). Fix them before running kbl-ingest.")
        }
    );
    s
}

#[cfg(test)]
mod tests;
