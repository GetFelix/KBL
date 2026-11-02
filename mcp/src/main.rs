//! `felix-mcp`: the tools one agent in a Felix Agents run is given, served as
//! MCP over stdio. The agent's token comes from the environment, so the agent
//! can do on Felix what its token allows and nothing else.
//!
//! Stdout belongs to the protocol. Anything meant for a person goes to stderr.

mod felix;
mod server;

use std::path::PathBuf;

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
    /// The agent's Felix token, issued for the broker.
    #[arg(long, env = "AGENTS_FELIX_TOKEN", hide_env_values = true)]
    token: String,
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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let run = RunId::new(cli.run).context("--run")?;
    let agent = AgentName::new(cli.agent).context("--agent")?;
    let felix = felix::connect(Target {
        tenant: cli.tenant,
        namespace: cli.namespace,
        broker: cli.broker,
        broker_ca: cli.broker_ca,
        broker_name: cli.broker_name,
        token: cli.token,
    })
    .await?;
    eprintln!(
        "felix-mcp: {} in run {}, connected to Felix",
        agent.as_str(),
        run.as_str()
    );

    let service = Server::new(run, agent, felix)
        .serve(stdio())
        .await
        .context("start the MCP server")?;
    service.waiting().await?;
    Ok(())
}
