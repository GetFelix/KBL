//! `source init`: writes a starter `ingest.json` for one source, asking only
//! for what the config cannot guess. See docs/ingest-setup.md.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand, ValueEnum};

const GITHUB: &str = include_str!("source/github.json");
const JIRA: &str = include_str!("source/jira.json");

#[derive(Subcommand)]
pub(crate) enum SourceCommand {
    /// Write a starter ingest config for a source.
    #[command(subcommand)]
    Init(InitCommand),
}

#[derive(Subcommand)]
pub(crate) enum InitCommand {
    /// Ingest the issues of one GitHub repository that carry a label.
    Github(GithubArgs),
    /// Ingest the issues one Jira JQL filter picks in one project.
    Jira(JiraArgs),
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Mode {
    /// A matching issue becomes a root at once.
    Admit,
    /// A matching issue waits in Triage until a person accepts it.
    Propose,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Admit => "admit",
            Mode::Propose => "propose",
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Triage {
    /// An admitted root starts with a triage item and an augment item.
    Run,
    /// The issue is already specified: the root is Ready to fix at once.
    Skip,
}

impl Triage {
    fn name(self) -> &'static str {
        match self {
            Triage::Run => "run",
            Triage::Skip => "skip",
        }
    }
}

#[derive(Args)]
pub(crate) struct Common {
    /// Write the config here instead of printing it. Refuses to overwrite a file.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Overwrite `--out` if it exists.
    #[arg(long, requires = "out")]
    force: bool,
    /// Where the tracker is.
    #[arg(long, default_value = "http://127.0.0.1:8790")]
    tracker_url: String,
    /// The label that marks an issue for agents. It names the rule too.
    #[arg(long, default_value = "agent-ok")]
    label: String,
    #[arg(long, value_enum, default_value = "admit")]
    mode: Mode,
    /// `skip` admits an already specified issue with no triage or augment item.
    /// Only with `--mode admit`.
    #[arg(long, value_enum, default_value = "run")]
    triage: Triage,
    /// The most open roots the rule may have at once.
    #[arg(long, default_value_t = 5)]
    cap: u32,
}

#[derive(Args)]
pub(crate) struct GithubArgs {
    /// The repository, `owner/name`. Asked for when missing and stdin is a terminal.
    #[arg(long)]
    repo: Option<String>,
    /// The environment variable that will hold the GitHub token.
    #[arg(long, default_value = "GITHUB_TOKEN")]
    token_env: String,
    #[command(flatten)]
    common: Common,
}

#[derive(Args)]
pub(crate) struct JiraArgs {
    /// The name rules use for the site, such as `acme`.
    #[arg(long)]
    site: Option<String>,
    /// The site's address, such as `https://acme.atlassian.net`.
    #[arg(long)]
    api: Option<String>,
    /// The Jira project key, such as `FEL`.
    #[arg(long)]
    project: Option<String>,
    /// The account the API token belongs to.
    #[arg(long)]
    email: Option<String>,
    /// The GitHub repository agents work in, `owner/name`.
    #[arg(long)]
    target_repo: Option<String>,
    /// The JQL filter, combined with the project. Defaults to the label.
    #[arg(long)]
    jql: Option<String>,
    /// The environment variable that will hold the Jira API token.
    #[arg(long, default_value = "JIRA_API_TOKEN")]
    token_env: String,
    #[command(flatten)]
    common: Common,
}

pub(crate) fn run(command: SourceCommand) -> Result<()> {
    let SourceCommand::Init(init) = command;
    let (text, common, token_env) = render(init, &mut Prompt::new())?;
    match &common.out {
        None => print!("{text}"),
        Some(path) => write(path, &text, common.force)?,
    }
    let at = common
        .out
        .as_deref()
        .map_or("ingest.json".to_owned(), |p| p.display().to_string());
    eprintln!(
        "\nNext: put the source token in {token_env} and ingest's tracker token in \
         KBL_INGEST_TOKEN, then run\n  kbl-ingest check --config {at}\n\
         It tests the credentials and lists what each rule would import, writing nothing."
    );
    Ok(())
}

/// The config text, the shared options, and the variable the source token goes in.
fn render(init: InitCommand, ask: &mut Prompt) -> Result<(String, Common, String)> {
    let common = match &init {
        InitCommand::Github(a) => &a.common,
        InitCommand::Jira(a) => &a.common,
    };
    if matches!(common.triage, Triage::Skip) && matches!(common.mode, Mode::Propose) {
        bail!("--triage skip needs --mode admit; a proposed issue waits for a person");
    }
    if matches!(common.triage, Triage::Skip) && common.label.len() > 120 {
        bail!("--triage skip records the rule name, which is the label: keep it to 120 bytes");
    }
    Ok(match init {
        InitCommand::Github(a) => {
            let repo = ask.value(a.repo, "repo", "The GitHub repository (owner/name)")?;
            owner_name(&repo, "--repo")?;
            let text = fill(
                GITHUB,
                &[
                    ("repo", &repo),
                    ("token_env", &a.token_env),
                    ("tracker_url", &a.common.tracker_url),
                    ("label", &a.common.label),
                    ("mode", a.common.mode.name()),
                    ("triage", a.common.triage.name()),
                ],
                a.common.cap,
            );
            check_json(&text)?;
            (text, a.common, a.token_env)
        }
        InitCommand::Jira(a) => {
            let site = ask.value(a.site, "site", "A name for the Jira site (such as acme)")?;
            let api = ask.value(
                a.api,
                "api",
                "The site's address (https://acme.atlassian.net)",
            )?;
            let project = ask.value(a.project, "project", "The Jira project key (such as FEL)")?;
            let email = ask.value(a.email, "email", "The account email of the API token")?;
            let target = ask.value(
                a.target_repo,
                "target-repo",
                "The GitHub repository agents work in (owner/name)",
            )?;
            if site.is_empty() || site.contains(['/', '#']) {
                bail!("--site is a plain name, without `/` or `#`");
            }
            if project.is_empty() || project.contains('/') {
                bail!("--project is a project key, such as FEL");
            }
            owner_name(&target, "--target-repo")?;
            let jql = a
                .jql
                .unwrap_or_else(|| format!("labels = \"{}\"", a.common.label.replace('"', "")));
            let repo = format!("{site}/{project}");
            let text = fill(
                JIRA,
                &[
                    ("site", &site),
                    ("api", &api),
                    ("email", &email),
                    ("token_env", &a.token_env),
                    ("tracker_url", &a.common.tracker_url),
                    ("label", &a.common.label),
                    ("repo", &repo),
                    ("jql", &jql),
                    ("mode", a.common.mode.name()),
                    ("triage", a.common.triage.name()),
                    ("target_repo", &target),
                ],
                a.common.cap,
            );
            check_json(&text)?;
            (text, a.common, a.token_env)
        }
    })
}

/// Whatever was substituted must still be one JSON document.
fn check_json(text: &str) -> Result<()> {
    serde_json::from_str::<serde_json::Value>(text).context("the starter config is not JSON")?;
    Ok(())
}

/// Replaces each `{{key}}` with the value as a JSON string, and `{{cap}}` as a number.
fn fill(template: &str, values: &[(&str, &str)], cap: u32) -> String {
    let mut text = template.replace("{{cap}}", &cap.to_string());
    for (key, value) in values {
        let json = serde_json::to_string(value).expect("a string is JSON");
        text = text.replace(&format!("{{{{{key}}}}}"), &json);
    }
    text
}

fn owner_name(s: &str, flag: &str) -> Result<()> {
    let ok = s
        .split_once('/')
        .is_some_and(|(o, n)| !o.is_empty() && !n.is_empty() && !n.contains('/'));
    if !ok {
        bail!("{flag} is owner/name, such as GetFelix/felix");
    }
    Ok(())
}

fn write(path: &Path, text: &str, force: bool) -> Result<()> {
    let mut f = std::fs::OpenOptions::new();
    f.write(true);
    if force {
        f.create(true).truncate(true);
    } else {
        f.create_new(true);
    }
    let mut f = f.open(path).with_context(|| {
        format!(
            "write {} (it must not exist unless you pass --force)",
            path.display()
        )
    })?;
    f.write_all(text.as_bytes())?;
    eprintln!("wrote {}", path.display());
    Ok(())
}

/// Asks for a missing value on a terminal, and otherwise names the flag.
struct Prompt {
    terminal: bool,
}

impl Prompt {
    fn new() -> Self {
        Self {
            terminal: std::io::stdin().is_terminal(),
        }
    }

    fn value(&mut self, given: Option<String>, flag: &str, question: &str) -> Result<String> {
        if let Some(v) = given.filter(|v| !v.trim().is_empty()) {
            return Ok(v.trim().to_owned());
        }
        if !self.terminal {
            bail!("pass --{flag}");
        }
        eprint!("{question}: ");
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        let line = line.trim();
        if line.is_empty() {
            bail!("--{flag} is required");
        }
        Ok(line.to_owned())
    }
}

#[cfg(test)]
mod tests;
