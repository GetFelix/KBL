//! `agent add`, `list` and `revoke`: one agent token per `kbl-mcp` session,
//! minted by the tracker with no restart. See docs/work-tracker.md, "Agents".

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use serde_json::{Value, json};

#[derive(Subcommand)]
pub(crate) enum AgentCommand {
    /// Mint a token for a new agent and print it. It is shown once.
    Add {
        /// The agent's name: letters, digits and `_`. A revoked name stays taken.
        name: String,
        #[command(flatten)]
        tracker: TrackerArgs,
    },
    /// List the team's agents and what each holds.
    List {
        #[command(flatten)]
        tracker: TrackerArgs,
    },
    /// Stop an agent's token and release what it holds.
    Revoke {
        name: String,
        #[command(flatten)]
        tracker: TrackerArgs,
    },
}

#[derive(Args)]
pub(crate) struct TrackerArgs {
    /// The team the agent works in. Defaults to `--namespace`.
    #[arg(long)]
    team: Option<String>,
    #[arg(long, env = "KBL_TRACKER_URL", default_value = "http://127.0.0.1:8790")]
    tracker_url: String,
    /// An organization admin's tracker token.
    #[arg(long, env = "KBL_TRACKER_TOKEN", hide_env_values = true)]
    tracker_token: String,
}

impl TrackerArgs {
    fn url(&self, namespace: &str, rest: &str) -> String {
        let team = self.team.as_deref().unwrap_or(namespace);
        format!(
            "{}/v1/teams/{team}/agents{rest}",
            self.tracker_url.trim_end_matches('/')
        )
    }
}

pub(crate) async fn run(namespace: &str, command: AgentCommand) -> Result<()> {
    let http = reqwest::Client::new();
    match command {
        AgentCommand::Add { name, tracker } => {
            let req = http
                .post(tracker.url(namespace, ""))
                .json(&json!({ "agent": name }));
            let body = send(req, &tracker).await?;
            eprintln!(
                "added agent {name} to team {}. Give this token to one kbl-mcp session as its KBL_TRACKER_TOKEN; it is not shown again:",
                body["team"].as_str().unwrap_or("?")
            );
            // The token alone on stdout, so `$(kbl-tool agent add w1)` captures it.
            println!("{}", body["token"].as_str().unwrap_or_default());
        }
        AgentCommand::List { tracker } => {
            let body = send(http.get(tracker.url(namespace, "")), &tracker).await?;
            for line in list_lines(&body) {
                println!("{line}");
            }
        }
        AgentCommand::Revoke { name, tracker } => {
            let body = send(
                http.delete(tracker.url(namespace, &format!("/{name}"))),
                &tracker,
            )
            .await?;
            match body["revoked"].as_bool() {
                Some(true) => println!("revoked {name}"),
                _ => println!("{name} was already revoked"),
            }
            if let Some(items) = body["released"].as_array() {
                let items: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
                if !items.is_empty() {
                    println!("  released {}", items.join(", "));
                }
            }
            if let Some(note) = body["note"].as_str() {
                println!("  {note}");
            }
        }
    }
    Ok(())
}

async fn send(req: reqwest::RequestBuilder, tracker: &TrackerArgs) -> Result<Value> {
    let response = req
        .bearer_auth(&tracker.tracker_token)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .context("could not reach the tracker")?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let error = body["error"].as_str().unwrap_or("no reason given");
        match body["why"].as_str() {
            Some(why) => bail!("the tracker refused: {status}: {error} ({why})"),
            None => bail!("the tracker refused: {status}: {error}"),
        }
    }
    Ok(body)
}

/// One line per agent: its name, then whether it is revoked or what it holds.
pub(crate) fn list_lines(body: &Value) -> Vec<String> {
    let empty = Vec::new();
    let agents = body["agents"].as_array().unwrap_or(&empty);
    if agents.is_empty() {
        return vec!["no agents".to_owned()];
    }
    agents
        .iter()
        .map(|a| {
            let name = a["agent"].as_str().unwrap_or("?");
            let from = if a["token_file"].as_bool() == Some(true) {
                " (token file)"
            } else {
                ""
            };
            let what = if a["revoked"].as_bool() == Some(true) {
                "revoked".to_owned()
            } else {
                match a["holds"].as_array() {
                    Some(h) if h.is_empty() => "holds nothing".to_owned(),
                    Some(h) => {
                        let ids: Vec<&str> = h.iter().filter_map(Value::as_str).collect();
                        format!("holds {}", ids.join(", "))
                    }
                    None => "holds unknown: the team is not serving".to_owned(),
                }
            };
            format!("{name}{from}  {what}")
        })
        .collect()
}
