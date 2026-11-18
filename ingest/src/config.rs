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
    /// Exactly one of `github`, `jira`, `ado` and `gitlab`: a worker serves
    /// one source.
    pub(crate) github: Option<GithubConfig>,
    pub(crate) jira: Option<JiraConfig>,
    pub(crate) ado: Option<AdoConfig>,
    pub(crate) gitlab: Option<GitlabConfig>,
    /// The least seconds between polls when no webhook wakes it, and never
    /// below 60. The worker stretches the gap when the request budget needs it.
    #[serde(default = "default_poll")]
    pub(crate) poll_seconds: u64,
    /// Hours for one pass that re-reads every open imported issue, to notice
    /// a transfer or a deletion, which no listing shows. Each poll reads its share.
    #[serde(default = "default_reconcile_hours")]
    pub(crate) reconcile_hours: u64,
    /// The removed polls-between-passes setting. It is read only to refuse
    /// the config with the replacement named.
    #[serde(default)]
    reconcile_every: Option<u64>,
    /// Requests per hour this worker may spend. Unset, it is `budget_share`
    /// of the limit the vendor reports. A worker that serves a connection
    /// takes its share of the pool's budget from the tracker instead.
    #[serde(default)]
    pub(crate) budget_per_hour: Option<u64>,
    /// The part of the vendor's reported hourly limit this worker may spend,
    /// when `budget_per_hour` is unset.
    #[serde(default = "default_budget_share")]
    pub(crate) budget_share: f64,
    /// The most release compares one poll makes looking for the release
    /// that carries a merged root. The rest wait for the next poll.
    #[serde(default = "default_release_compares")]
    pub(crate) release_compares_per_poll: u32,
    pub(crate) rules: Vec<Rule>,
}

fn default_poll() -> u64 {
    300
}

fn default_reconcile_hours() -> u64 {
    24
}

/// Half the limit the vendor reports. GitHub counts per account, so a personal
/// token shares it with the person's own tools; a bot account or App can go higher.
fn default_budget_share() -> f64 {
    0.5
}

/// The least gap between interval polls, whatever `poll_seconds` says.
const MIN_POLL_SECONDS: u64 = 60;

fn default_release_compares() -> u32 {
    20
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrackerConfig {
    pub(crate) url: String,
    /// The environment variable holding ingest's tracker token.
    pub(crate) token_env: String,
    /// The worker group this worker serves. Set, the worker takes its sources
    /// by lease once a connection names the group; unset, it polls every
    /// configured repo as before.
    #[serde(default)]
    pub(crate) group: Option<String>,
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

/// How ingest signs in to Azure DevOps. The secret is read from the
/// environment or a file, never from this config, and only ingest holds it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AdoConfig {
    /// The Azure DevOps organization. Rule repos are `<organization>/<project>`.
    pub(crate) organization: String,
    /// The service address; `dev.azure.com` unless it is a proxy or a server.
    #[serde(default = "default_ado_api")]
    pub(crate) api: String,
    /// Rule-repo project name to the project's real name, for a name that has
    /// spaces or other characters an item id cannot hold.
    #[serde(default)]
    pub(crate) projects: BTreeMap<String, String>,
    /// Personal access token sign-in. It needs Work Items (Read), and
    /// (Read & write) only when a rule writes back.
    pub(crate) pat_env: Option<String>,
    pub(crate) pat_file: Option<std::path::PathBuf>,
    /// Microsoft Entra service principal sign-in (client credentials).
    pub(crate) entra: Option<EntraConfig>,
    /// Display names the credential writes as.
    #[serde(default)]
    pub(crate) own_logins: Vec<String>,
    /// Where the agents' pull requests live, for the link written back.
    #[serde(default = "default_pr_base")]
    pub(crate) pr_url_base: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EntraConfig {
    pub(crate) tenant_id: String,
    pub(crate) client_id: String,
    pub(crate) client_secret_env: Option<String>,
    pub(crate) client_secret_file: Option<std::path::PathBuf>,
    /// The sign-in service; `login.microsoftonline.com` unless a sovereign cloud.
    #[serde(default = "default_authority")]
    pub(crate) authority: String,
}

/// How ingest signs in to GitLab. The token is read from the environment or a
/// file, never from this config, and only ingest holds it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitlabConfig {
    /// The instance's address. Set it for a self-managed GitLab. Plain http
    /// is refused except on loopback.
    #[serde(default = "default_gitlab_api")]
    pub(crate) api: String,
    /// Rule-repo `group/project` to the project's full path, for a project in
    /// a subgroup (`group/sub/project`), which an issue id cannot hold.
    #[serde(default)]
    pub(crate) projects: BTreeMap<String, String>,
    /// A group or project access token (a bot user) with `read_api`, and
    /// `api` only where a rule writes back.
    pub(crate) token_env: Option<String>,
    pub(crate) token_file: Option<std::path::PathBuf>,
    /// Usernames the credential writes as, besides the one GitLab reports.
    #[serde(default)]
    pub(crate) own_logins: Vec<String>,
}

fn default_gitlab_api() -> String {
    "https://gitlab.com".into()
}

fn default_ado_api() -> String {
    "https://dev.azure.com".into()
}

fn default_authority() -> String {
    "https://login.microsoftonline.com".into()
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

/// What an admitted root does before it can be fixed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Triage {
    /// The root starts with a triage item and an augment item.
    #[default]
    Run,
    /// The issue is already specified: the root goes straight to Ready to fix
    /// with no children.
    Skip,
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
    /// `skip` admits a match with no triage or augment item. Only with `admit`.
    #[serde(default)]
    pub(crate) triage: Triage,
    /// Jira: the JQL filter, combined with the project in `repo`
    /// (`site/PROJ`).
    pub(crate) jql: Option<String>,
    /// Jira: component name to the repository agents work in. The first
    /// listed component with a mapping wins; `target_repo` is the fallback.
    #[serde(default)]
    pub(crate) components: BTreeMap<String, String>,
    /// Azure DevOps: a saved query, by id or path (`Shared Queries/Agent`),
    /// that picks the work items. Without it the tag filters above decide.
    pub(crate) ado_query: Option<String>,
    /// Azure DevOps: area path to the repository agents work in. An item
    /// takes the longest listed path that is its area or an ancestor of it;
    /// `target_repo` is the fallback.
    #[serde(default)]
    pub(crate) area_paths: BTreeMap<String, String>,
    /// Set when the worker's source is Azure DevOps.
    #[serde(skip)]
    pub(crate) ado: bool,
    /// Set when the worker's source is GitLab.
    #[serde(skip)]
    pub(crate) gitlab: bool,
    /// Jira, Azure DevOps and GitLab: comment on the issue when an agent's pull
    /// request is linked.
    #[serde(default)]
    pub(crate) comment: bool,
    /// Jira and Azure DevOps: add the pull request as a link on the issue.
    #[serde(default)]
    pub(crate) link_pr: bool,
    /// Jira: the workflow transition to apply when a pull request is linked
    /// (for example "In Review") and when it merges (for example "Done").
    /// Skipped when the workflow does not offer it. Azure DevOps: the state
    /// to set, skipped when the work item's process refuses it.
    pub(crate) transition_on_pr: Option<String>,
    pub(crate) transition_on_merge: Option<String>,
    /// Open roots this rule may have at once.
    pub(crate) cap: Option<u32>,
    #[serde(default = "default_tier")]
    pub(crate) default_tier: u8,
    /// Label to tier. The lowest tier among an issue's labels wins.
    #[serde(default)]
    pub(crate) priority_labels: BTreeMap<String, u8>,
    /// Milestone (fix version, iteration) to tier. With `priority_labels`
    /// the lowest tier wins.
    #[serde(default)]
    pub(crate) milestone_tiers: BTreeMap<String, u8>,
    /// Label (tag) to the item's type or area, set when the issue is taken
    /// and again when its labels change. The value must be in the team's
    /// vocabulary or the tracker refuses it. A person's or triage's choice is kept.
    #[serde(default)]
    pub(crate) label_types: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) label_areas: BTreeMap<String, String>,
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
    /// The source's vendor word, as its sources' ids start: `github` in
    /// `github:o/r`. The same as the adapter's `Source::name`.
    pub(crate) fn vendor(&self) -> &'static str {
        match (&self.github, &self.jira, &self.ado, &self.gitlab) {
            (_, Some(_), _, _) => "jira",
            (_, _, Some(_), _) => "ado",
            (_, _, _, Some(_)) => "gitlab",
            _ => "github",
        }
    }

    /// The pass length of the spread reconcile.
    pub(crate) fn reconcile_period(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.reconcile_hours * 3600)
    }

    /// The least gap between interval polls: `poll_seconds`, but not under a minute.
    pub(crate) fn poll_floor(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.poll_seconds.max(MIN_POLL_SECONDS))
    }

    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read the config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parse the config {}", path.display()))
    }

    pub(crate) fn parse(text: &str) -> Result<Self> {
        let mut cfg: Config = serde_json::from_str(text)?;
        ensure!(!cfg.rules.is_empty(), "there are no intake rules");
        ensure!(cfg.poll_seconds >= 1, "poll_seconds must be at least 1");
        if cfg.poll_seconds < MIN_POLL_SECONDS {
            eprintln!(
                "kbl-ingest: warning: poll_seconds {} is below the {MIN_POLL_SECONDS}s floor; polling every {MIN_POLL_SECONDS}s (a webhook still wakes a poll sooner)",
                cfg.poll_seconds
            );
        }
        if let Some(n) = cfg.reconcile_every {
            // Not mapped on its own: the old pass was every N polls, and the
            // poll gap now stretches with the budget, so no fixed hours match.
            let hours = (n.saturating_mul(cfg.poll_seconds)).div_ceil(3600).max(1);
            anyhow::bail!(
                "reconcile_every is no longer accepted; use reconcile_hours (hours for one pass over every open imported issue, default 24). \
                 {n} polls of {}s is about reconcile_hours: {hours}",
                cfg.poll_seconds
            );
        }
        ensure!(
            cfg.reconcile_hours >= 1,
            "reconcile_hours must be at least 1"
        );
        ensure!(
            cfg.budget_per_hour != Some(0),
            "budget_per_hour must be above 0"
        );
        ensure!(
            cfg.budget_share > 0.0 && cfg.budget_share <= 1.0,
            "budget_share must be above 0 and at most 1"
        );
        match (&cfg.github, &cfg.jira, &cfg.ado, &cfg.gitlab) {
            (Some(g), None, None, None) => ensure!(
                g.token_file.is_some() || g.app.is_some() || g.token_env.is_some(),
                "github needs a token_file, an app or a token_env"
            ),
            (None, Some(j), None, None) => cfg.check_jira(j)?,
            (None, None, Some(a), None) => cfg.check_ado(a)?,
            (None, None, None, Some(g)) => cfg.check_gitlab(g)?,
            _ => anyhow::bail!("set exactly one of github, jira, ado and gitlab"),
        }
        let (is_ado, is_gitlab) = (cfg.ado.is_some(), cfg.gitlab.is_some());
        for r in &mut cfg.rules {
            r.ado = is_ado;
            r.gitlab = is_gitlab;
        }
        let mut seen = std::collections::HashSet::new();
        for r in &cfg.rules {
            ensure!(seen.insert(&r.name), "rule {:?} is listed twice", r.name);
            // The tracker keeps the name as the reason triage was skipped and
            // refuses a longer one, which would leave the root unaccepted.
            ensure!(
                r.triage == Triage::Run || r.name.len() <= 120,
                "rule {:?}: a name over 120 bytes cannot be used with triage \"skip\"",
                r.name
            );
            ensure!(
                r.triage == Triage::Run || r.mode == Mode::Admit,
                "rule {:?}: triage \"skip\" is for mode \"admit\"; a proposed issue waits for a person",
                r.name
            );
            let write_back = r.comment
                || r.link_pr
                || r.transition_on_pr.is_some()
                || r.transition_on_merge.is_some();
            ensure!(
                cfg.jira.is_some() || (r.jql.is_none() && r.components.is_empty()),
                "rule {:?}: jql and components are for jira",
                r.name
            );
            ensure!(
                cfg.ado.is_some() || (r.ado_query.is_none() && r.area_paths.is_empty()),
                "rule {:?}: ado_query and area_paths are for ado",
                r.name
            );
            ensure!(
                cfg.github.is_none() || !write_back,
                "rule {:?}: comment, link_pr and transitions are for jira, ado and gitlab",
                r.name
            );
            if let Some(g) = &cfg.gitlab {
                ensure!(
                    !r.link_pr,
                    "rule {:?}: link_pr is not for gitlab; the comment names the merge request",
                    r.name
                );
                ensure!(
                    [&r.transition_on_pr, &r.transition_on_merge]
                        .into_iter()
                        .flatten()
                        .all(|t| matches!(t.as_str(), "close" | "reopen")),
                    "rule {:?}: a gitlab transition is close or reopen",
                    r.name
                );
                ensure!(
                    crate::source::IssueRef::parse(&format!("{}#1", r.repo)).is_some()
                        && crate::gitlab::project_path(&g.projects, &r.repo).is_some(),
                    "rule {:?}: repo is group/project, with the full path of a subgroup \
                     project under gitlab.projects",
                    r.name
                );
            }
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
            if let Some(a) = &cfg.ado {
                let project = r.repo.strip_prefix(&format!("{}/", a.organization));
                ensure!(
                    project.is_some_and(|p| !p.is_empty() && !p.contains('/')),
                    "rule {:?}: repo is {}/PROJECT for this organization",
                    r.name,
                    a.organization
                );
                if let Some(q) = &r.ado_query {
                    ensure!(
                        !q.trim().is_empty()
                            && !q.chars().any(char::is_control)
                            && !q.split('/').any(|seg| seg == ".." || seg == "."),
                        "rule {:?}: ado_query is a query id or a path such as Shared Queries/Agent",
                        r.name
                    );
                }
                ensure!(
                    r.area_paths
                        .keys()
                        .all(|k| !k.trim().is_empty() && !k.chars().any(char::is_control)),
                    "rule {:?}: an area path is not empty",
                    r.name
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
                        .chain(r.milestone_tiers.values())
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

    fn check_ado(&self, a: &AdoConfig) -> Result<()> {
        ensure!(
            !a.organization.is_empty() && !a.organization.contains(['/', '#']),
            "ado.organization is a plain name"
        );
        let pat = a.pat_env.is_some() || a.pat_file.is_some();
        ensure!(
            pat != a.entra.is_some(),
            "ado signs in with pat_env or pat_file, or with entra, not both"
        );
        if let Some(e) = &a.entra {
            ensure!(
                e.client_secret_env.is_some() != e.client_secret_file.is_some(),
                "ado.entra needs exactly one of client_secret_env and client_secret_file"
            );
            ensure!(
                !e.tenant_id.is_empty()
                    && e.tenant_id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')),
                "ado.entra.tenant_id is a tenant id or domain"
            );
            ensure!(!e.client_id.is_empty(), "ado.entra.client_id is empty");
        }
        Ok(())
    }

    fn check_gitlab(&self, g: &GitlabConfig) -> Result<()> {
        crate::gitlab::base_url(&g.api).map_err(|e| anyhow::anyhow!("gitlab.api: {e}"))?;
        ensure!(
            g.token_env.is_some() != g.token_file.is_some(),
            "gitlab needs exactly one of token_env and token_file"
        );
        for (alias, real) in &g.projects {
            ensure!(
                crate::gitlab::project_path(&g.projects, alias).is_some()
                    && crate::source::IssueRef::parse(&format!("{alias}#1")).is_some(),
                "gitlab.projects: {alias:?} is group/project and {real:?} a project path"
            );
        }
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
