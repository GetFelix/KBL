//! The ingest worker's settings: where the tracker is, how to sign in to
//! GitHub, and the intake rules.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub(crate) tracker: TrackerConfig,
    /// Exactly one of `github` and `jira`: a worker serves one source.
    pub(crate) github: Option<GithubConfig>,
    pub(crate) jira: Option<JiraConfig>,
    /// Seconds between polls when no webhook wakes it.
    #[serde(default = "default_poll")]
    pub(crate) poll_seconds: u64,
    /// Every this many polls, fetch each open imported issue to notice a
    /// transfer or a deletion, which no listing shows.
    #[serde(default = "default_reconcile")]
    pub(crate) reconcile_every: u32,
    /// The most release compares one poll makes looking for the release
    /// that carries a merged root. The rest wait for the next poll.
    #[serde(default = "default_release_compares")]
    pub(crate) release_compares_per_poll: u32,
    pub(crate) rules: Vec<Rule>,
}

fn default_poll() -> u64 {
    60
}

fn default_reconcile() -> u32 {
    20
}

fn default_release_compares() -> u32 {
    20
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrackerConfig {
    pub(crate) url: String,
    /// The environment variable holding ingest's tracker token.
    pub(crate) token_env: String,
}

/// How ingest signs in to Jira. The secret is read from the environment or a
/// file, never from this config, and only ingest holds it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JiraConfig {
    /// The name rule repos and upstream ids use for the site: `site/PROJ`.
    pub(crate) site: String,
    /// The site, such as `https://acme.atlassian.net`. With `oauth` it is
    /// `https://api.atlassian.com/ex/jira/<cloud id>`.
    pub(crate) api: String,
    /// The site's browser address, the base of issue links. Defaults to
    /// `api`; set it with `oauth`, whose `api` is not a site people open.
    pub(crate) browse_url: Option<String>,
    /// API token sign-in: the account email and the token.
    pub(crate) email: Option<String>,
    pub(crate) token_env: Option<String>,
    pub(crate) token_file: Option<std::path::PathBuf>,
    /// OAuth app sign-in: a bearer token in a file that something beside the
    /// worker refreshes. Read again for every request.
    pub(crate) oauth_token_file: Option<std::path::PathBuf>,
    /// Display names or account ids the credential writes as.
    #[serde(default)]
    pub(crate) own_logins: Vec<String>,
    /// Where the agents' pull requests live, for the link written back.
    #[serde(default = "default_pr_base")]
    pub(crate) pr_url_base: String,
}

/// Whether JQL has an ORDER BY outside its quoted strings. Ingest appends
/// its own, and Jira rejects a search with two.
fn has_order_by(jql: &str) -> bool {
    let mut quote = None;
    let mut out = String::new();
    for c in jql.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None => out.push(c),
        }
    }
    let words: Vec<String> = out
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    words.windows(2).any(|w| w[0] == "order" && w[1] == "by")
}

fn default_pr_base() -> String {
    "https://github.com".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GithubConfig {
    #[serde(default = "default_api")]
    pub(crate) api: String,
    /// A file holding a token, read again for every request so an App
    /// installation token can be refreshed beside the worker.
    pub(crate) token_file: Option<std::path::PathBuf>,
    /// A GitHub App ingest mints installation tokens for itself.
    pub(crate) app: Option<AppConfig>,
    pub(crate) token_env: Option<String>,
    /// Logins the credential writes as, such as `kbl[bot]`.
    #[serde(default)]
    pub(crate) own_logins: Vec<String>,
}

/// Which installation of the App ingest acts as.
#[derive(Debug, Clone)]
pub(crate) enum Installation {
    Id(u64),
    /// Looked up from this `owner/name` repository's installation.
    Repo(String),
    /// Looked up from this organization's installation.
    Org(String),
}

/// A GitHub App: its id, the installation to act as, and the file holding
/// its private key. The key itself never enters the config.
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "RawApp")]
pub(crate) struct AppConfig {
    pub(crate) id: String,
    pub(crate) installation: Installation,
    pub(crate) private_key_file: std::path::PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawApp {
    /// The App id or client id; GitHub accepts either as the JWT issuer.
    id: serde_json::Value,
    installation_id: Option<u64>,
    installation_repo: Option<String>,
    installation_org: Option<String>,
    private_key_file: std::path::PathBuf,
}

impl TryFrom<RawApp> for AppConfig {
    type Error = String;

    fn try_from(r: RawApp) -> Result<Self, String> {
        let id = match &r.id {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) if !s.is_empty() => s.clone(),
            _ => return Err("github.app.id is the App's id".into()),
        };
        let installation = match (r.installation_id, r.installation_repo, r.installation_org) {
            (Some(i), None, None) => Installation::Id(i),
            (None, Some(repo), None) if repo.contains('/') => Installation::Repo(repo),
            (None, None, Some(org)) if !org.contains('/') => Installation::Org(org),
            _ => {
                return Err("github.app needs exactly one of installation_id, \
                            installation_repo (owner/name) or installation_org"
                    .into());
            }
        };
        Ok(Self {
            id,
            installation,
            private_key_file: r.private_key_file,
        })
    }
}

fn default_api() -> String {
    "https://api.github.com".into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mode {
    /// A matching issue becomes a root at once.
    Admit,
    /// A matching issue waits in Triage until a person accepts it.
    Propose,
}

/// One intake rule: GitHub's filter, combined with AND, plus what to do with a match.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Rule {
    pub(crate) name: String,
    pub(crate) repo: String,
    /// Every one of these labels.
    #[serde(default)]
    pub(crate) labels_all: Vec<String>,
    /// At least one of these, when any are listed.
    #[serde(default)]
    pub(crate) labels_any: Vec<String>,
    #[serde(default)]
    pub(crate) labels_excluded: Vec<String>,
    pub(crate) milestone: Option<String>,
    pub(crate) assignee: Option<String>,
    #[serde(rename = "type")]
    pub(crate) issue_type: Option<String>,
    pub(crate) mode: Mode,
    /// Jira: the JQL filter, combined with the project in `repo`
    /// (`site/PROJ`).
    pub(crate) jql: Option<String>,
    /// Jira: component name to the repository agents work in. The first
    /// listed component with a mapping wins; `target_repo` is the fallback.
    #[serde(default)]
    pub(crate) components: BTreeMap<String, String>,
    /// Jira: comment on the issue when an agent's pull request is linked.
    #[serde(default)]
    pub(crate) comment: bool,
    /// Jira: add the pull request as a remote link on the issue.
    #[serde(default)]
    pub(crate) link_pr: bool,
    /// Jira: the workflow transition to apply when a pull request is linked
    /// (for example "In Review") and when it merges (for example "Done").
    /// Skipped when the workflow does not offer it.
    pub(crate) transition_on_pr: Option<String>,
    pub(crate) transition_on_merge: Option<String>,
    /// Open roots this rule may have at once.
    pub(crate) cap: Option<u32>,
    #[serde(default = "default_tier")]
    pub(crate) default_tier: u8,
    /// Label to tier. The lowest tier among an issue's labels wins.
    #[serde(default)]
    pub(crate) priority_labels: BTreeMap<String, u8>,
    /// The repository agents work in; the issue's own when unset.
    pub(crate) target_repo: Option<String>,
    /// Ingest opens an issue in `repo` for a prerequisite that targets it,
    /// once a person accepts the prerequisite. Needs issues: write there.
    #[serde(default)]
    pub(crate) open_prerequisites: bool,
    /// The opened issue names the issue waiting on it. Off by default, so a
    /// private repo's issue is never named in a public one.
    #[serde(default)]
    pub(crate) link_waiter: bool,
}

fn default_tier() -> u8 {
    2
}

impl Config {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read the config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parse the config {}", path.display()))
    }

    pub(crate) fn parse(text: &str) -> Result<Self> {
        let cfg: Config = serde_json::from_str(text)?;
        ensure!(!cfg.rules.is_empty(), "there are no intake rules");
        match (&cfg.github, &cfg.jira) {
            (Some(g), None) => ensure!(
                g.token_file.is_some() || g.app.is_some() || g.token_env.is_some(),
                "github needs a token_file, an app or a token_env"
            ),
            (None, Some(j)) => cfg.check_jira(j)?,
            _ => anyhow::bail!("set exactly one of github and jira"),
        }
        let mut seen = std::collections::HashSet::new();
        for r in &cfg.rules {
            ensure!(seen.insert(&r.name), "rule {:?} is listed twice", r.name);
            let jira_only = r.jql.is_some()
                || !r.components.is_empty()
                || r.comment
                || r.link_pr
                || r.transition_on_pr.is_some()
                || r.transition_on_merge.is_some();
            ensure!(
                cfg.jira.is_some() || !jira_only,
                "rule {:?}: jql, components, comment, link_pr and transitions are for jira",
                r.name
            );
            if let Some(j) = &cfg.jira {
                ensure!(
                    r.jql.is_some(),
                    "rule {:?}: a jira rule needs a jql filter",
                    r.name
                );
                if let Some(jql) = &r.jql {
                    ensure!(
                        !has_order_by(jql),
                        "rule {:?}: jql must not have ORDER BY; ingest orders the search itself",
                        r.name
                    );
                }
                let project = r.repo.strip_prefix(&format!("{}/", j.site));
                ensure!(
                    project.is_some_and(|p| !p.is_empty() && !p.contains('/')),
                    "rule {:?}: repo is {}/PROJECT for this site",
                    r.name,
                    j.site
                );
            }
            ensure!(
                r.repo.contains('/'),
                "rule {:?}: repo is owner/name",
                r.name
            );
            ensure!(
                r.default_tier < kbl_core::work::TIERS
                    && r.priority_labels
                        .values()
                        .all(|t| *t < kbl_core::work::TIERS),
                "rule {:?}: a tier is 0 to 3",
                r.name
            );
        }
        Ok(cfg)
    }

    fn check_jira(&self, j: &JiraConfig) -> Result<()> {
        ensure!(
            !j.site.is_empty() && !j.site.contains(['/', '#']),
            "jira.site is a plain name"
        );
        let token = j.token_env.is_some() || j.token_file.is_some();
        ensure!(
            (token && j.email.is_some() && j.oauth_token_file.is_none())
                || (!token && j.email.is_none() && j.oauth_token_file.is_some()),
            "jira signs in with email plus token_env or token_file, or with oauth_token_file"
        );
        Ok(())
    }

    /// The repositories the rules read.
    pub(crate) fn repos(&self) -> Vec<String> {
        let mut repos: Vec<String> = self.rules.iter().map(|r| r.repo.clone()).collect();
        repos.sort();
        repos.dedup();
        repos
    }
}

#[cfg(test)]
mod tests;
