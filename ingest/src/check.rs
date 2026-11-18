//! `kbl-ingest check`: what an operator needs to know before starting the
//! worker. It parses the config with the worker's own parser, tests each
//! credential against its vendor, reports the permissions every rule needs,
//! and lists what each rule would import on a first poll.
//!
//! It reads only. The vendor transport refuses any request that is not a GET
//! and the tracker is called through `roots` and `load`, so a check cannot
//! change either side. Azure DevOps has one exception, a query (see
//! [`ReadOnlyWiql`]). An App or Entra credential still mints its token,
//! which is how it is tested. Output names environment variables and files,
//! never their contents.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde_json::Value;

use crate::ado::Ado;
use crate::budget::{Budget, Budgeted, Vendor, budgeted, projected_per_hour};
use crate::config::{AdoConfig, Config, GithubConfig, Mode, Rule, Triage};
use crate::github::transport::Reqwest as GithubHttp;
use crate::github::{Github, Req, Res, Transport, parse_utc_ms};
use crate::gitlab::Gitlab;
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
    /// The rule admits straight to Ready to fix, with no triage item.
    pub(crate) skips_triage: bool,
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

/// A [`ReadOnly`] that also lets through Azure DevOps's work item query.
///
/// WIQL is only ever sent as a POST of a query, and the service has no GET
/// form that takes the query text. That one request reads and changes nothing,
/// so it is the only non-GET allowed: a POST to exactly
/// `{org}[/{project}]/_apis/wit/wiql` whose body is a lone `SELECT` query.
/// Any other method, path or body is refused as in `ReadOnly`.
pub(crate) struct ReadOnlyWiql<T>(pub(crate) T);

impl<T: Transport> Transport for ReadOnlyWiql<T> {
    async fn send(&self, req: Req) -> Result<Res> {
        if req.method != "GET" && !is_wiql_query(&req) {
            return Err(anyhow!(
                "check does not write: refused a {} request",
                req.method
            ));
        }
        self.0.send(req).await
    }
}

fn is_wiql_query(req: &Req) -> bool {
    if req.method != "POST" {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(&req.url) else {
        return false;
    };
    let segments: Vec<&str> = url.path_segments().map_or_else(Vec::new, Iterator::collect);
    let Some(tail) = segments.len().checked_sub(3).map(|n| &segments[n..]) else {
        return false;
    };
    let Some(body) = req.body.as_ref().and_then(Value::as_object) else {
        return false;
    };
    tail == ["_apis", "wit", "wiql"]
        && body.len() == 1
        && body["query"]
            .as_str()
            .is_some_and(|q| q.trim_start().to_ascii_uppercase().starts_with("SELECT "))
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
            source_name(&cfg)
        ),
    ));
    report
        .findings
        .extend(static_findings(&cfg, &|n| std::env::var(n).ok()));

    let budget = Budget::new(
        Vendor::named(source_name(&cfg)),
        cfg.budget_per_hour,
        cfg.budget_share,
    );
    let tracker = match std::env::var(&cfg.tracker.token_env) {
        Ok(token) if !token.is_empty() => Http::new(&cfg.tracker.url, token).ok(),
        _ => None,
    };
    match (&cfg.github, &cfg.jira) {
        (Some(g), None) => match crate::github_token(g, &budget).and_then(GithubHttp::new) {
            Ok(http) => {
                let mut github = Github::new(
                    &g.api,
                    ReadOnly(budgeted(http, budget.clone())),
                    g.own_logins.clone(),
                );
                // As the worker does, so its own issues are recognised.
                if let Some(login) = github.login().await {
                    github.add_own(login);
                }
                github_findings(&cfg, g, &github, &mut report.findings).await;
                dry_run(&cfg, &github, tracker.as_ref(), &mut report).await;
                let f = budget_finding(&cfg, &github, tracker.as_ref(), &budget).await;
                report.findings.push(f);
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
                    ReadOnly(budgeted(http, budget.clone())),
                    j.own_logins.clone(),
                    crate::jira_scopes(&cfg),
                    &j.pr_url_base,
                );
                jira_findings(&cfg, &mut jira, &mut report.findings).await;
                dry_run(&cfg, &jira, tracker.as_ref(), &mut report).await;
                let f = budget_finding(&cfg, &jira, tracker.as_ref(), &budget).await;
                report.findings.push(f);
            }
            Err(e) => {
                report
                    .findings
                    .push(Finding::new(Level::Error, "jira", safe(format!("{e:#}"))))
            }
        },
        (None, None) => {
            if let Some(a) = &cfg.ado {
                match ado_guarded(&cfg, a, &budget) {
                    Ok(mut ado) => {
                        ado_findings(&cfg, &mut ado, &mut report.findings).await;
                        dry_run(&cfg, &ado, tracker.as_ref(), &mut report).await;
                        let f = budget_finding(&cfg, &ado, tracker.as_ref(), &budget).await;
                        report.findings.push(f);
                    }
                    Err(e) => report.findings.push(Finding::new(
                        Level::Error,
                        "ado",
                        safe(format!("{e:#}")),
                    )),
                }
            } else if let Some(g) = &cfg.gitlab {
                match gitlab_guarded(&cfg, g, &budget) {
                    Ok(mut gitlab) => {
                        gitlab_findings(&cfg, &mut gitlab, &mut report.findings).await;
                        dry_run(&cfg, &gitlab, tracker.as_ref(), &mut report).await;
                        let f = budget_finding(&cfg, &gitlab, tracker.as_ref(), &budget).await;
                        report.findings.push(f);
                    }
                    Err(e) => report.findings.push(Finding::new(
                        Level::Error,
                        "gitlab",
                        safe(format!("{e:#}")),
                    )),
                }
            }
        }
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

/// The Azure DevOps source as check uses it. The return type is the guarantee
/// that no request leaves except through [`ReadOnlyWiql`].
fn ado_guarded(
    cfg: &Config,
    a: &AdoConfig,
    budget: &Arc<Budget>,
) -> Result<Ado<ReadOnlyWiql<Budgeted<crate::ado::transport::Reqwest>>>> {
    let http = crate::ado::transport::Reqwest::new(crate::ado_auth(a, budget))?;
    Ok(Ado::new(
        &a.organization,
        &a.api,
        ReadOnlyWiql(budgeted(http, budget.clone())),
        a.own_logins.clone(),
        crate::ado_scopes(cfg),
        &a.projects,
        &a.pr_url_base,
    ))
}

/// The GitLab source as check uses it, behind [`ReadOnly`] by type.
fn gitlab_guarded(
    cfg: &Config,
    g: &crate::config::GitlabConfig,
    budget: &Arc<Budget>,
) -> Result<Gitlab<ReadOnly<Budgeted<crate::gitlab::transport::Reqwest>>>> {
    let http = crate::gitlab::transport::Reqwest::new(crate::gitlab_secret(g))?;
    Gitlab::new(
        &g.api,
        ReadOnly(budgeted(http, budget.clone())),
        g.own_logins.clone(),
        cfg.rules.clone(),
        &g.projects,
    )
}

/// What the vendor says is left, what this worker may spend, and what the
/// config is projected to spend an hour, so a config that cannot fit shows
/// before it runs.
async fn budget_finding<S: Source, T: Tracker>(
    cfg: &Config,
    source: &S,
    tracker: Option<&T>,
    budget: &Budget,
) -> Finding {
    let snap = budget.snapshot();
    let mut roots = 0u64;
    if let Some(t) = tracker
        && let Ok(listed) = t.roots().await
    {
        roots = listed
            .roots
            .iter()
            .filter(|r| r.source == source.name() && !r.closed)
            .count() as u64;
    }
    let listings = 2 * cfg.repos().len() as u64;
    let reads = if source.reads_pull_requests() {
        roots
    } else {
        0
    };
    let every = cfg.poll_floor();
    let projected = projected_per_hour(listings, reads, roots, every, cfg.reconcile_period());
    let seen = match (snap.vendor_remaining, snap.vendor_hourly) {
        (Some(left), Some(limit)) => format!("the vendor reports {left} of {limit} requests left"),
        (Some(left), None) => format!("the vendor reports {left} requests left"),
        _ => "the vendor reports no remaining count".to_owned(),
    };
    let reset = snap
        .vendor_reset_epoch
        .map(|r| r.saturating_sub(epoch_secs()))
        .map_or(String::new(), |s| format!(", resetting in {} min", s / 60));
    let usable = snap.hourly as f64 * 0.8;
    let mut message = format!(
        "{seen}{reset}; this worker may spend {} an hour; the config is projected to spend about {projected:.0} an hour \
         ({listings} listing calls and {reads} pull request reads a poll every {}s, {roots} open issues re-read every {}h)",
        snap.hourly,
        every.as_secs(),
        cfg.reconcile_hours
    );
    let level = if projected > usable {
        message.push_str(
            "; that is over budget, so the worker will stretch its poll interval; \
             raise poll_seconds or budget_per_hour, or use a token of its own",
        );
        Level::Warn
    } else {
        Level::Ok
    };
    Finding::new(level, "budget", message)
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn source_name(cfg: &Config) -> &'static str {
    match (&cfg.github, &cfg.jira, &cfg.ado) {
        (Some(_), ..) => "github",
        (_, Some(_), _) => "jira",
        (.., Some(_)) => "ado",
        _ => "gitlab",
    }
}

async fn tracker_finding<T: Tracker>(cfg: &Config, tracker: &T) -> Finding {
    match tracker.roots().await {
        Ok(_) => Finding::new(
            Level::Ok,
            "tracker",
            format!("{} answers the ingest token", safe(&cfg.tracker.url)),
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
                safe(format!("api {} is not https", g.api)),
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
                    Err(e) => Finding::new(Level::Error, "github", safe(format!("{e:#}"))),
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
    if let Some(a) = &cfg.ado {
        out.extend(ado_static(a, &var));
    }
    if let Some(g) = &cfg.gitlab {
        if let Some(name) = &g.token_env {
            out.push(var("gitlab", name));
        }
        if let Some(f) = &g.token_file {
            out.push(secret_file("gitlab", "token_file", f));
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

fn ado_static(a: &AdoConfig, var: &dyn Fn(&str, &str) -> Finding) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Some(name) = &a.pat_env {
        out.push(var("ado", name));
    }
    if let Some(f) = &a.pat_file {
        out.push(secret_file("ado", "pat_file", f));
    }
    if let Some(e) = &a.entra {
        if let Some(name) = &e.client_secret_env {
            out.push(var("ado", name));
        }
        if let Some(f) = &e.client_secret_file {
            out.push(secret_file("ado", "entra.client_secret_file", f));
        }
    }
    for url in std::iter::once(&a.api).chain(a.entra.iter().map(|e| &e.authority)) {
        if !url.starts_with("https://") {
            out.push(Finding::new(
                Level::Warn,
                "ado",
                safe(format!("address {url} is not https")),
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
        // Only an App installation token has no user behind it. A token that
        // GitHub will not name was refused or cannot see its owner.
        Ok(res) if gh.app.is_some() && matches!(res.status, 403 | 404) => {
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
        Ok(res) if matches!(res.status, 403 | 404) => out.push(Finding::new(
            Level::Error,
            "github",
            format!(
                "GitHub answered {} to the sign-in test: the token is not accepted as a user token (wrong, revoked, or missing the access to name its owner). For an App, configure `app` instead of a token",
                res.status
            ),
        )),
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
        out.push(Finding::new(Level::Error, "jira", safe(format!("{e:#}"))));
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

/// Work item rights, as the organization's permission service reports them.
/// Classification nodes hold work item read (16) and write (32).
const CSS_NAMESPACE: &str = "83e28ad4-2d72-4ceb-97b0-c7726d5502c3";
const WORK_ITEM_WRITE: &str = "32";

fn ado_rule_writes(r: &Rule) -> bool {
    r.comment || r.link_pr || r.transition_on_pr.is_some() || r.transition_on_merge.is_some()
}

/// Azure DevOps: sign-in, then per rule the project read and, where the rule
/// writes back, the work item edit right.
pub(crate) async fn ado_findings<T: Transport>(
    cfg: &Config,
    ado: &mut Ado<T>,
    out: &mut Vec<Finding>,
) {
    match ado.probe(None, "/connectionData", &[], "7.1-preview").await {
        Ok(res) if res.status == 200 => {
            let user = &res.body["authenticatedUser"];
            let names: Vec<String> = ["providerDisplayName", "customDisplayName"]
                .iter()
                .filter_map(|k| user[k].as_str().map(str::to_owned))
                .collect();
            out.push(Finding::new(
                Level::Ok,
                "ado",
                format!(
                    "the credential signs in as {}",
                    names.first().map_or("an identity", String::as_str)
                ),
            ));
            ado.add_own(names);
        }
        Ok(res) if matches!(res.status, 401 | 403) => {
            out.push(Finding::new(
                Level::Error,
                "ado",
                format!(
                    "Azure DevOps refused the credential ({}): the personal access token is wrong, \
                     expired or revoked, or the service principal is not a member of the organization",
                    res.status
                ),
            ));
            return;
        }
        Ok(res) => out.push(Finding::new(
            Level::Error,
            "ado",
            format!("Azure DevOps answered {} to the sign-in test", res.status),
        )),
        Err(e) => {
            out.push(Finding::new(Level::Error, "ado", safe(e)));
            return;
        }
    }
    if let Err(e) = ado.require_own() {
        out.push(Finding::new(Level::Error, "ado", safe(format!("{e:#}"))));
    }

    // Per project: whether it reads, and its work item write answer, once.
    let mut seen: BTreeMap<&str, bool> = BTreeMap::new();
    let mut writes: BTreeMap<&str, Option<bool>> = BTreeMap::new();
    for r in &cfg.rules {
        let Some(project) = ado.project_name(&r.repo).map(str::to_owned) else {
            continue;
        };
        let scope = format!("repo {}", r.repo);
        if !seen.contains_key(r.repo.as_str()) {
            let mut readable = false;
            // Area nodes are work item data, so a Work Items (Read) token reads them.
            match ado
                .probe(
                    Some(&project),
                    "/wit/classificationnodes/areas",
                    &[("$depth", "0")],
                    "7.1",
                )
                .await
            {
                Ok(res) if res.status == 200 => {
                    readable = true;
                    out.push(Finding::new(
                        Level::Ok,
                        &scope,
                        format!("the credential can read work items in {project}"),
                    ));
                    let need_write = cfg
                        .rules
                        .iter()
                        .any(|x| x.repo == r.repo && ado_rule_writes(x));
                    if need_write {
                        let node = res.body["identifier"].as_str().unwrap_or_default();
                        writes.insert(&r.repo, ado_can_write(ado, node).await);
                    }
                }
                Ok(res) if matches!(res.status, 401 | 403 | 404) => out.push(Finding::new(
                    Level::Error,
                    &scope,
                    format!(
                        "Azure DevOps answered {} reading project {project}: it does not exist, or the \
                         credential cannot see it (grant Work Items: Read)",
                        res.status
                    ),
                )),
                Ok(res) => out.push(Finding::new(
                    Level::Error,
                    &scope,
                    format!("Azure DevOps answered {} reading project {project}", res.status),
                )),
                Err(e) => out.push(Finding::new(Level::Error, &scope, safe(e))),
            }
            seen.insert(&r.repo, readable);
        }
        if !seen[r.repo.as_str()] {
            continue;
        }
        let rule_scope = format!("rule {}", r.name);
        out.push(Finding::new(
            Level::Ok,
            &rule_scope,
            format!("work items: read in {project}"),
        ));
        if !ado_rule_writes(r) {
            continue;
        }
        out.push(match writes.get(r.repo.as_str()).copied().flatten() {
            Some(true) => Finding::new(
                Level::Ok,
                &rule_scope,
                format!("work items: may edit in {project}"),
            ),
            Some(false) => Finding::new(
                Level::Error,
                &rule_scope,
                format!(
                    "the credential may not edit work items in {project}; comment, link_pr and \
                     transitions need Work Items: Read & write"
                ),
            ),
            // A personal access token's scopes are not visible to the API.
            None => Finding::new(
                Level::Warn,
                &rule_scope,
                format!(
                    "could not tell whether the credential may edit work items in {project}; \
                     comment, link_pr and transitions need Work Items: Read & write"
                ),
            ),
        });
    }
}

/// Whether the credential may edit work items under an area node, or None
/// when the permission service would not say.
async fn ado_can_write<T: Transport>(ado: &Ado<T>, node: &str) -> Option<bool> {
    if node.is_empty() {
        return None;
    }
    let token = format!("vstfs:///Classification/Node/{node}");
    let path = format!("/permissions/{CSS_NAMESPACE}/{WORK_ITEM_WRITE}");
    let res = ado
        .probe(None, &path, &[("tokens", &token)], "7.1")
        .await
        .ok()?;
    (res.status == 200)
        .then(|| res.body["value"][0].as_bool())
        .flatten()
}

/// GitLab access levels: Guest, Reporter.
const GUEST: u64 = 10;
const REPORTER: u64 = 20;

/// The access level a rule's writes need: a comment or opening an issue takes
/// a Guest, a close or reopen takes a Reporter.
fn gitlab_write_level(r: &Rule) -> Option<u64> {
    if r.transition_on_pr.is_some() || r.transition_on_merge.is_some() {
        Some(REPORTER)
    } else if r.comment || r.open_prerequisites {
        Some(GUEST)
    } else {
        None
    }
}

/// GitLab: sign-in, the token's scopes and expiry where the API says them,
/// then per rule the project read and the role its writes need.
pub(crate) async fn gitlab_findings<T: Transport>(
    cfg: &Config,
    gitlab: &mut Gitlab<T>,
    out: &mut Vec<Finding>,
) {
    match gitlab.probe(&["user"], &[]).await {
        Ok(res) if res.status == 200 => {
            let who = res.body["username"].as_str().unwrap_or("a user");
            out.push(Finding::new(
                Level::Ok,
                "gitlab",
                format!("the token signs in as {who}"),
            ));
            gitlab.add_own(vec![who.to_owned()]);
        }
        Ok(res) if res.status == 401 => {
            out.push(Finding::new(
                Level::Error,
                "gitlab",
                "GitLab rejected the token (401): it is wrong, expired or revoked",
            ));
            return;
        }
        Ok(res) => out.push(Finding::new(
            Level::Warn,
            "gitlab",
            format!(
                "GitLab answered {} to GET /user, so it did not say who the token is",
                res.status
            ),
        )),
        Err(e) => {
            out.push(Finding::new(Level::Error, "gitlab", safe(e)));
            return;
        }
    }
    if let Err(e) = gitlab.require_own() {
        out.push(Finding::new(Level::Error, "gitlab", safe(format!("{e:#}"))));
    }
    let has_api = gitlab_token_findings(gitlab, out).await;

    let mut seen: BTreeMap<&str, Option<u64>> = BTreeMap::new();
    for r in &cfg.rules {
        let Ok(project) = gitlab.project_of(&r.repo).map(str::to_owned) else {
            continue;
        };
        let scope = format!("repo {}", r.repo);
        if !seen.contains_key(r.repo.as_str()) {
            let level = match gitlab.probe(&["projects", &project], &[]).await {
                Ok(res) if res.status == 200 => {
                    out.push(Finding::new(
                        Level::Ok,
                        &scope,
                        format!("the token can read {project}"),
                    ));
                    let p = &res.body["permissions"];
                    let level = ["project_access", "group_access"]
                        .iter()
                        .filter_map(|k| p[k]["access_level"].as_u64())
                        .max();
                    // No role is listed for an admin, a member through a shared group or
                    // a visitor of a public project, so nothing is known.
                    Some(level.unwrap_or(u64::MAX))
                }
                Ok(res) if matches!(res.status, 401 | 403 | 404) => {
                    out.push(Finding::new(
                        Level::Error,
                        &scope,
                        format!(
                            "GitLab answered {} reading {project}: it does not exist, or the token \
                             cannot see it (grant read_api and a role in the project)",
                            res.status
                        ),
                    ));
                    None
                }
                Ok(res) => {
                    out.push(Finding::new(
                        Level::Error,
                        &scope,
                        format!("GitLab answered {} reading {project}", res.status),
                    ));
                    None
                }
                Err(e) => {
                    out.push(Finding::new(Level::Error, &scope, safe(e)));
                    None
                }
            };
            seen.insert(&r.repo, level);
        }
        let Some(Some(level)) = seen.get(r.repo.as_str()).copied() else {
            continue;
        };
        let rule_scope = format!("rule {}", r.name);
        out.push(Finding::new(
            Level::Ok,
            &rule_scope,
            format!("issues: read in {project}"),
        ));
        let Some(need) = gitlab_write_level(r) else {
            continue;
        };
        let role = if need == GUEST { "Guest" } else { "Reporter" };
        out.push(if level == u64::MAX {
            Finding::new(
                Level::Warn,
                &rule_scope,
                format!(
                    "could not tell the token's role in {project}; the rule's writes need {role} or higher"
                ),
            )
        } else if level >= need {
            Finding::new(
                Level::Ok,
                &rule_scope,
                format!("role in {project} is enough to write ({role} or higher)"),
            )
        } else {
            Finding::new(
                Level::Error,
                &rule_scope,
                format!(
                    "the token's role in {project} is below {role}, which the rule's writes (comment, \
                     transitions, open_prerequisites) need"
                ),
            )
        });
        if has_api == Some(false) {
            out.push(Finding::new(
                Level::Error,
                &rule_scope,
                "the token lacks the api scope, which the rule's writes need; read_api only reads",
            ));
        }
    }
}

/// The token's own scopes and expiry. Returns whether it holds `api`, or None
/// when GitLab would not say.
async fn gitlab_token_findings<T: Transport>(
    gitlab: &Gitlab<T>,
    out: &mut Vec<Finding>,
) -> Option<bool> {
    let res = match gitlab.probe(&["personal_access_tokens", "self"], &[]).await {
        Ok(res) => res,
        Err(e) => {
            out.push(Finding::new(Level::Error, "gitlab", safe(e)));
            return None;
        }
    };
    match res.status {
        200 => {}
        401 => {
            out.push(Finding::new(
                Level::Error,
                "gitlab",
                "GitLab rejected the token (401): it is wrong, expired or revoked",
            ));
            return None;
        }
        s => {
            out.push(Finding::new(
                Level::Warn,
                "gitlab",
                format!(
                    "GitLab answered {s} for the token's own details (it needs GitLab 15.5 or later \
                     and an access token), so its scopes and expiry are not checked"
                ),
            ));
            return None;
        }
    }
    let body = &res.body;
    let scopes: Vec<&str> = body["scopes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let api = scopes.contains(&"api");
    if body["revoked"] == Value::Bool(true) || body["active"] == Value::Bool(false) {
        out.push(Finding::new(
            Level::Error,
            "gitlab",
            "the token is revoked or no longer active",
        ));
    }
    out.push(if api || scopes.contains(&"read_api") {
        Finding::new(
            Level::Ok,
            "gitlab",
            format!("token scopes: {}", scopes.join(", ")),
        )
    } else {
        Finding::new(
            Level::Error,
            "gitlab",
            format!(
                "token scopes are [{}]: it needs read_api, or api where a rule writes back",
                scopes.join(", ")
            ),
        )
    });
    match body["expires_at"].as_str() {
        None => out.push(Finding::new(
            Level::Ok,
            "gitlab",
            "the token does not expire",
        )),
        Some(date) => {
            let at = parse_utc_ms(&format!("{date}T00:00:00Z"));
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64);
            match at {
                Some(at) if at <= now => out.push(Finding::new(
                    Level::Error,
                    "gitlab",
                    format!("the token expired on {date}"),
                )),
                Some(at) if at - now < 14 * 86_400_000 => out.push(Finding::new(
                    Level::Warn,
                    "gitlab",
                    format!("the token expires on {date}"),
                )),
                _ => out.push(Finding::new(
                    Level::Ok,
                    "gitlab",
                    format!("the token expires on {date}"),
                )),
            }
        }
    }
    Some(api)
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
                "the source answered {code} listing its issues; for GitHub, Azure DevOps and GitLab the \
                 credential cannot read them, for Jira a 400 is a JQL it rejects and a 401 or 403 is no access"
            ),
        ));
    }

    let mut imported: BTreeSet<String> = BTreeSet::new();
    let mut open: BTreeMap<String, usize> = BTreeMap::new();
    let mut known = false;
    if let Some(t) = tracker
        && let Ok(listed) = t.roots().await
    {
        known = true;
        let state = t
            .load(source.name())
            .await
            .ok()
            .and_then(|(v, _)| v)
            .and_then(|v| serde_json::from_value::<State>(v).ok())
            .unwrap_or_default();
        // As the worker reads them: this source's roots, with a moved issue's new id aliased.
        let mut roots: std::collections::HashMap<String, _> = listed
            .roots
            .into_iter()
            .filter(|r| r.source == source.name())
            .map(|r| (r.upstream_id.clone(), r))
            .collect();
        crate::sync::alias_moved(&state, &mut roots);
        imported = roots.keys().cloned().collect();
        // The worker's own count: items a rule admitted whose root is still open.
        for (key, tracked) in &state.items {
            if tracked.moved_to.is_none() && roots.get(key).is_some_and(|r| !r.closed) {
                *open.entry(tracked.rule.clone()).or_default() += 1;
            }
        }
    }
    if !known {
        report.findings.push(Finding::new(
            Level::Warn,
            "dry run",
            "the tracker's roots could not be read, so the dry run ignores what is already imported and each rule's open roots",
        ));
    }
    let own: Vec<String> = source
        .own_logins()
        .iter()
        .map(|l| l.to_ascii_lowercase())
        .collect();

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
            skips_triage: r.triage == Triage::Skip,
            cap: r.cap,
            open_now: open.get(&r.name).copied().unwrap_or(0),
            would_import: Vec::new(),
            over_cap: Vec::new(),
            already_imported: 0,
        })
        .collect();
    for issue in changes.issues.iter().filter(|i| i.closed.is_none()) {
        // The worker never imports an issue it opened for a prerequisite.
        if is_prerequisite(cfg, &own, issue) {
            continue;
        }
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

/// An issue ingest opened for a prerequisite: its own author, in a repo a rule
/// opens prerequisites in, with the marker line.
fn is_prerequisite(cfg: &Config, own: &[String], issue: &crate::source::Issue) -> bool {
    own.contains(&issue.author.to_ascii_lowercase())
        && cfg
            .rules
            .iter()
            .any(|r| r.open_prerequisites && r.repo.eq_ignore_ascii_case(&issue.id.repo))
        && issue
            .body
            .lines()
            .any(|l| l.trim().starts_with(crate::sync::MARKER))
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
        if rule.skips_triage {
            let _ = writeln!(
                s,
                "  admits straight to ready: no triage or augment item, so an agent can fix it at once"
            );
        }
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
