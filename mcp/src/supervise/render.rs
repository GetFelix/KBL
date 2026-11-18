//! Filling a profile's templates and writing a worker's MCP config. The
//! command stays an argv array that runs without a shell, and each value is
//! checked before it is put in, so a value can never become a second argument
//! or a flag.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::config::{Format, check_template};

/// The values a template may be filled with.
pub(crate) struct Vars<'a> {
    pub(crate) model: Option<&'a str>,
    pub(crate) worker: &'a str,
    pub(crate) mcp_config_path: Option<&'a str>,
    pub(crate) workdir: Option<&'a str>,
}

impl Vars<'_> {
    fn get(&self, name: &str) -> Result<&str> {
        match name {
            "model" => self.model.context(
                "this launch has no model; set one on the profile, the fit row or the call",
            ),
            "worker" => Ok(self.worker),
            "mcp_config_path" => self.mcp_config_path.context("no MCP config path here"),
            "workdir" => self.workdir.context("the profile has no workdir"),
            _ => bail!("{{{name}}} is not a placeholder"),
        }
    }
}

/// `t` with each `{name}` in `allowed` replaced. A value never holds a brace
/// itself, so a filled value is not read again as a placeholder.
pub(crate) fn fill(t: &str, allowed: &[&str], vars: &Vars<'_>) -> Result<String> {
    check_template(t, allowed)?;
    let mut out = String::with_capacity(t.len());
    let mut rest = t;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after.find('}').expect("checked above");
        let value = vars.get(&after[..close])?;
        if value.contains(['{', '}', '\0']) {
            bail!("a value for {{{}}} holds a brace or a NUL", &after[..close]);
        }
        out.push_str(value);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The worker's MCP config: one server, `kbl`, that starts `kbl_mcp` with `env`.
pub(crate) fn mcp_config(
    format: Format,
    kbl_mcp: &Path,
    env: &BTreeMap<String, String>,
) -> Result<String> {
    let command = kbl_mcp.to_str().context("the kbl-mcp path is not UTF-8")?;
    let server = json!({ "command": command, "args": [], "env": env });
    Ok(match format {
        Format::Json => serde_json::to_string_pretty(&json!({ "mcpServers": { "kbl": server } }))?,
        Format::Toml => toml::to_string(&json!({ "mcp_servers": { "kbl": server } }))?,
    })
}

/// Makes `dir` and any parents, the last one mode 700.
pub(crate) fn private_dir(dir: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("create {}", dir.display()))
}

/// Writes `text` to a new file only this user can read. An existing file or
/// link at `path` is an error, so a token is never written through one.
pub(crate) fn write_private(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        private_dir(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

/// `p` made absolute against the current directory, since a worker runs elsewhere.
pub(crate) fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_owned()
    } else {
        std::env::current_dir().map_or_else(|_| p.to_owned(), |d| d.join(p))
    }
}

#[cfg(test)]
mod tests;
