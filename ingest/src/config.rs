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
    pub(crate) github: GithubConfig,
    /// Seconds between polls when no webhook wakes it.
    #[serde(default = "default_poll")]
    pub(crate) poll_seconds: u64,
    /// Every this many polls, fetch each open imported issue to notice a
    /// transfer or a deletion, which no listing shows.
    #[serde(default = "default_reconcile")]
    pub(crate) reconcile_every: u32,
    pub(crate) rules: Vec<Rule>,
}

fn default_poll() -> u64 {
    60
}

fn default_reconcile() -> u32 {
    20
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TrackerConfig {
    pub(crate) url: String,
    /// The environment variable holding ingest's tracker token.
    pub(crate) token_env: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GithubConfig {
    #[serde(default = "default_api")]
    pub(crate) api: String,
    /// A file holding a token, read again for every request so an App
    /// installation token can be refreshed beside the worker.
    pub(crate) token_file: Option<std::path::PathBuf>,
    pub(crate) token_env: Option<String>,
    /// Logins the credential writes as, such as `kbl[bot]`.
    #[serde(default)]
    pub(crate) own_logins: Vec<String>,
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
    /// Open roots this rule may have at once.
    pub(crate) cap: Option<u32>,
    #[serde(default = "default_tier")]
    pub(crate) default_tier: u8,
    /// Label to tier. The lowest tier among an issue's labels wins.
    #[serde(default)]
    pub(crate) priority_labels: BTreeMap<String, u8>,
    /// The repository agents work in; the issue's own when unset.
    pub(crate) target_repo: Option<String>,
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
        ensure!(
            cfg.github.token_file.is_some() || cfg.github.token_env.is_some(),
            "github needs a token_file or a token_env"
        );
        let mut seen = std::collections::HashSet::new();
        for r in &cfg.rules {
            ensure!(seen.insert(&r.name), "rule {:?} is listed twice", r.name);
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
