//! `felix-mcp`: the tools one agent in a Felix Agents run is given, served as
//! MCP over stdio. The agent's token comes from the environment, so the agent
//! can do on Felix what its token allows and nothing else.
//!
//! Stdout belongs to the protocol. Anything meant for a person goes to stderr.

mod felix;
mod server;
mod tasks;

use std::path::PathBuf;
use std::time::Duration;

use agents_core::names::{AgentName, RunId};
use anyhow::{Context, Result};
use clap::Parser;
use rmcp::{ServiceExt, transport::stdio};

use felix::Target;
use server::Server;

#[derive(Parser)]
#[command(version, about = "Felix tools for one agent, served as MCP over stdio")]
struct Cli {
    /// The run this agent works in.
    #[arg(long, env = "AGENTS_RUN")]
    run: String,
    /// This agent's name in the run, such as `planner` or `w1`.
    #[arg(long, env = "AGENTS_AGENT")]
    agent: String,
    #[arg(long, env = "AGENTS_TENANT", default_value = "agents")]
    tenant: String,
    #[arg(long, env = "AGENTS_NAMESPACE", default_value = "default")]
    namespace: String,
    /// The broker's QUIC address.
    #[arg(long, env = "AGENTS_FELIX_BROKER", default_value = "127.0.0.1:5000")]
    broker: String,
    /// The PEM file the broker's certificate is trusted from.
    #[arg(
        long,
        env = "AGENTS_FELIX_CA_FILE",
        default_value = "dev/state/broker-cert.pem"
    )]
    broker_ca: PathBuf,
    /// The name in the broker's certificate.
    #[arg(long, env = "AGENTS_FELIX_BROKER_NAME", default_value = "localhost")]
    broker_name: String,
    /// How long a claim stands without an extension, in milliseconds. For tests.
    #[arg(long, env = "AGENTS_CLAIM_VISIBILITY_MS", hide = true)]
    claim_visibility_ms: Option<u64>,
    /// How often a held claim is extended, in milliseconds. For tests.
    #[arg(long, env = "AGENTS_EXTEND_EVERY_MS", hide = true)]
    extend_every_ms: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // Environment only: a flag would show the token in `ps`.
    let token = std::env::var("AGENTS_FELIX_TOKEN")
        .context("set AGENTS_FELIX_TOKEN to the agent's Felix token for the broker")?;
    let run = RunId::new(cli.run).context("--run")?;
    let agent = AgentName::new(cli.agent).context("--agent")?;
    let felix = felix::connect(Target {
        tenant: cli.tenant,
        namespace: cli.namespace,
        broker: cli.broker,
        broker_ca: cli.broker_ca,
        broker_name: cli.broker_name,
        token,
    })
    .await?;
    eprintln!(
        "felix-mcp: {} in run {}, connected to Felix",
        agent.as_str(),
        run.as_str()
    );

    let timing = server::Timing {
        visibility: cli.claim_visibility_ms.map(Duration::from_millis),
        extend_every: cli.extend_every_ms.map(Duration::from_millis),
    };
    let service = Server::new(run, agent, felix, timing)
        .serve(stdio())
        .await
        .context("start the MCP server")?;
    service.waiting().await?;
    Ok(())
}
