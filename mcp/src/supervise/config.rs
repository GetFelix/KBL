//! The supervisor's config: launch profiles and the fit table. Everything a
//! template can expand to is checked here or when it is filled, so nothing
//! read from a work item ever reaches a command line.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

/// The placeholders a command may hold.
pub(crate) const COMMAND_VARS: [&str; 4] = ["model", "worker", "mcp_config_path", "workdir"];
/// The placeholders a path or a `set_env` value may hold.
pub(crate) const PATH_VARS: [&str; 2] = ["worker", "workdir"];
/// The secrets a supervisor holds that a worker must never be handed.
const SECRETS: [&str; 3] = ["KBL_TRACKER_TOKEN", "KBL_PERSON_TOKEN", "KBL_FELIX_TOKEN"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    /// Where per-worker MCP configs and output go.
    pub(crate) state_dir: Option<PathBuf>,
    /// The `kbl-mcp` a worker's MCP config starts. Defaults to this binary.
    pub(crate) kbl_mcp: Option<PathBuf>,
    pub(crate) default_profile: Option<String>,
    #[serde(default = "default_top_up")]
    pub(crate) top_up_seconds: u64,
    /// A worker that exits sooner than this counts toward pausing the top-up.
    #[serde(default = "default_min_run")]
    pub(crate) min_run_seconds: u64,
    /// Copies a fixed `KBL_FELIX_TOKEN` into each worker's MCP config.
    #[serde(default)]
    pub(crate) share_felix_token: bool,
    pub(crate) profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub(crate) fit: Vec<FitRow>,
}

fn default_top_up() -> u64 {
    30
}

fn default_min_run() -> u64 {
    30
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Command,
    Hook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Format {
    #[default]
    Json,
    Toml,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Profile {
    pub(crate) kind: Kind,
    pub(crate) vendor: String,
    /// The model when neither the fit nor the launch names one.
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) command: Vec<String>,
    #[serde(default)]
    pub(crate) mcp_config: Format,
    /// Where the worker's MCP config is written, when the vendor reads it from a fixed place.
    pub(crate) mcp_config_file: Option<String>,
    pub(crate) workdir: Option<String>,
    #[serde(default)]
    pub(crate) pass_env: Vec<String>,
    #[serde(default)]
    pub(crate) set_env: BTreeMap<String, String>,
    /// The name and email the worker's commits carry, set as the four `GIT_*` variables.
    pub(crate) git_identity: Option<GitIdentity>,
    pub(crate) url: Option<String>,
    /// The supervisor's variable holding a bearer for the hook.
    pub(crate) hook_token_env: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitIdentity {
    pub(crate) name: String,
    pub(crate) email: String,
}

impl GitIdentity {
    fn check(&self) -> Result<()> {
        let bad = |s: &str| s.chars().any(char::is_control) || s.len() > 254;
        if self.name.trim().is_empty() || bad(&self.name) {
            bail!("git_identity name must be 1 to 254 characters with no control characters");
        }
        let ok = !bad(&self.email)
            && self.email.split_once('@').is_some_and(|(local, domain)| {
                !local.is_empty()
                    && !domain.contains('@')
                    && domain.contains('.')
                    && !domain.starts_with('.')
                    && !domain.ends_with('.')
            })
            && !self.email.contains(char::is_whitespace);
        if !ok {
            bail!(
                "git_identity email {:?} is not an email address",
                self.email
            );
        }
        Ok(())
    }

    /// The four variables git reads for the author and the committer.
    pub(crate) fn env(&self) -> [(&'static str, &str); 4] {
        [
            ("GIT_AUTHOR_NAME", &self.name),
            ("GIT_AUTHOR_EMAIL", &self.email),
            ("GIT_COMMITTER_NAME", &self.name),
            ("GIT_COMMITTER_EMAIL", &self.email),
        ]
    }
}

/// One row of the fit table. Every field it lists must match; a list matches
/// any of its values.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FitRow {
    pub(crate) tiers: Option<Vec<u8>>,
    pub(crate) kinds: Option<Vec<String>>,
    pub(crate) types: Option<Vec<String>>,
    pub(crate) areas: Option<Vec<String>>,
    pub(crate) sizes: Option<Vec<String>>,
    /// Matches an item that needs any of these.
    pub(crate) needs: Option<Vec<String>>,
    pub(crate) profile: String,
    pub(crate) model: Option<String>,
}

/// The recorded fields of a ready item, as `work_peek` lists them.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Fields {
    pub(crate) tier: Option<u8>,
    pub(crate) kind: Option<String>,
    pub(crate) item_type: Option<String>,
    pub(crate) area: Option<String>,
    pub(crate) size: Option<String>,
    pub(crate) needs: Vec<String>,
}

impl Fields {
    pub(crate) fn of_peeked(item: &Value) -> Self {
        let text = |k: &str| item[k].as_str().map(str::to_owned);
        Self {
            tier: item["tier"].as_u64().and_then(|t| u8::try_from(t).ok()),
            kind: text("work"),
            item_type: text("type"),
            area: text("area"),
            size: text("size"),
            needs: item["needs"]
                .as_array()
                .map(|n| {
                    n.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

impl Config {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read the supervise config {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("the supervise config {}", path.display()))
    }

    pub(crate) fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        config.check()?;
        Ok(config)
    }

    pub(crate) fn top_up_every(&self) -> Duration {
        Duration::from_secs(self.top_up_seconds)
    }

    fn check(&self) -> Result<()> {
        if !(5..=3600).contains(&self.top_up_seconds) {
            bail!("top_up_seconds must be 5 to 3600");
        }
        if self.min_run_seconds > 3600 {
            bail!("min_run_seconds must be 0 to 3600");
        }
        if self.profiles.is_empty() {
            bail!("name at least one profile");
        }
        for (name, p) in &self.profiles {
            p.check(name).with_context(|| format!("profile {name}"))?;
        }
        if let Some(d) = &self.default_profile {
            self.profile(d).context("default_profile")?;
        }
        for (n, row) in self.fit.iter().enumerate() {
            self.profile(&row.profile)
                .with_context(|| format!("fit row {}", n + 1))?;
            if let Some(m) = &row.model {
                check_model(m).with_context(|| format!("fit row {}", n + 1))?;
            }
        }
        Ok(())
    }

    pub(crate) fn profile(&self, name: &str) -> Result<&Profile> {
        self.profiles
            .get(name)
            .with_context(|| format!("no profile is named {name:?}"))
    }

    /// The profile and model for an item: the first fit row that matches,
    /// else the default profile. None when nothing applies.
    pub(crate) fn fit(&self, item: Option<&Fields>) -> Option<(&str, Option<&str>)> {
        let row = item.and_then(|i| self.fit.iter().find(|r| r.matches(i)));
        match row {
            Some(r) => Some((r.profile.as_str(), r.model.as_deref())),
            None => self.default_profile.as_deref().map(|p| (p, None)),
        }
    }
}

impl FitRow {
    fn matches(&self, item: &Fields) -> bool {
        let one = |want: &Option<Vec<String>>, got: &Option<String>| {
            want.as_ref().is_none_or(|w| {
                got.as_ref()
                    .is_some_and(|g| w.iter().any(|v| v.eq_ignore_ascii_case(g)))
            })
        };
        self.tiers
            .as_ref()
            .is_none_or(|t| item.tier.is_some_and(|i| t.contains(&i)))
            && one(&self.kinds, &item.kind)
            && one(&self.types, &item.item_type)
            && one(&self.areas, &item.area)
            && one(&self.sizes, &item.size)
            && self.needs.as_ref().is_none_or(|w| {
                w.iter()
                    .any(|v| item.needs.iter().any(|n| n.eq_ignore_ascii_case(v)))
            })
    }
}

impl Profile {
    fn check(&self, name: &str) -> Result<()> {
        check_label("profile name", name)?;
        check_label("vendor", &self.vendor)?;
        if let Some(m) = &self.model {
            check_model(m)?;
        }
        for v in &self.pass_env {
            check_env_name(v, "pass_env")?;
        }
        for (k, v) in &self.set_env {
            check_env_name(k, "set_env")?;
            check_template(v, &PATH_VARS).with_context(|| format!("set_env {k}"))?;
        }
        if let Some(g) = &self.git_identity {
            g.check()?;
        }
        match self.kind {
            Kind::Command => {
                if self.url.is_some() || self.hook_token_env.is_some() {
                    bail!("url and hook_token_env are for kind = \"hook\"");
                }
                let Some((program, args)) = self.command.split_first() else {
                    bail!("command needs at least the program");
                };
                if program.is_empty() || program.contains(['{', '}']) {
                    bail!("the program (the first element of command) takes no placeholder");
                }
                for a in args {
                    check_template(a, &COMMAND_VARS).context("command")?;
                }
                if let Some(w) = &self.workdir {
                    check_template(w, &["worker"]).context("workdir")?;
                }
                if let Some(f) = &self.mcp_config_file {
                    check_template(f, &PATH_VARS).context("mcp_config_file")?;
                }
            }
            Kind::Hook => {
                if !self.command.is_empty()
                    || self.workdir.is_some()
                    || self.mcp_config_file.is_some()
                    || !self.pass_env.is_empty()
                    || !self.set_env.is_empty()
                    || self.git_identity.is_some()
                {
                    bail!(
                        "command, workdir, mcp_config_file, pass_env, set_env and git_identity are for kind = \"command\""
                    );
                }
                let url = self.url.as_deref().context("a hook needs url")?;
                let parsed = url::Url::parse(url).context("url")?;
                let local = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
                if parsed.scheme() != "https" && !(parsed.scheme() == "http" && local) {
                    bail!("a hook url is https, or http on loopback");
                }
                if let Some(e) = &self.hook_token_env
                    && SECRETS.contains(&e.as_str())
                {
                    bail!("hook_token_env may not be {e}");
                }
            }
        }
        Ok(())
    }

    /// The variables the worker process gets beyond `pass_env`: `git_identity` first, then
    /// `set_env`, so a `set_env` entry wins over the identity.
    pub(crate) fn fixed_env(&self) -> BTreeMap<String, String> {
        let mut env: BTreeMap<String, String> = self
            .git_identity
            .iter()
            .flat_map(GitIdentity::env)
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        env.extend(self.set_env.clone());
        env
    }

    /// Whether the command fills `{model}`, so a launch without one must fail.
    pub(crate) fn needs_model(&self) -> bool {
        self.command.iter().any(|a| a.contains("{model}"))
    }
}

fn check_label(what: &str, v: &str) -> Result<()> {
    if v.is_empty()
        || v.len() > 64
        || !v
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("{what} {v:?} must be 1 to 64 of A-Z a-z 0-9 _ -");
    }
    Ok(())
}

/// A model name fills an argv element, so it may not look like a flag or
/// carry anything a vendor's CLI could read as more than a name.
pub(crate) fn check_model(m: &str) -> Result<()> {
    let ok = !m.is_empty()
        && m.len() <= 64
        && !m.starts_with('-')
        && m.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/@-".contains(c));
    if !ok {
        bail!("model {m:?} must be 1 to 64 of A-Z a-z 0-9 . _ : / @ - and not start with -");
    }
    Ok(())
}

/// A worker's KBL settings come only from its MCP config, so no `KBL_` name
/// may be passed or set on its process.
fn check_env_name(v: &str, what: &str) -> Result<()> {
    if v.is_empty() || !v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("{what}: {v:?} is not a variable name");
    }
    if v.to_ascii_uppercase().starts_with("KBL_") {
        bail!("{what}: {v} is not passed to a worker; its KBL settings come from its MCP config");
    }
    Ok(())
}

/// The placeholder names in `t`, or an error for a brace that is not one of `allowed`.
pub(crate) fn check_template(t: &str, allowed: &[&str]) -> Result<()> {
    let mut rest = t;
    while let Some(open) = rest.find(['{', '}']) {
        if rest[open..].starts_with('}') {
            bail!("{t:?} has a stray }}");
        }
        let after = &rest[open + 1..];
        let close = after
            .find('}')
            .with_context(|| format!("{t:?} has an unclosed {{"))?;
        let name = &after[..close];
        if !allowed.contains(&name) {
            bail!(
                "{t:?} uses {{{name}}}; only {} may be used here",
                allowed
                    .iter()
                    .map(|a| format!("{{{a}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        rest = &after[close + 1..];
    }
    Ok(())
}

#[cfg(test)]
mod tests;
