//! `kbl-mcp`: the tools one agent in a Felix Agents run is given, served as
//! MCP over stdio. The agent's token comes from the environment, so the agent
//! can do on Felix what its token allows and nothing else.
//!
//! Stdout belongs to the protocol. Anything meant for a person goes to stderr.

mod felix;
mod keepalive;
mod metrics;
mod server;
mod tasks;
mod work;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use kbl_core::names::{AgentName, RunId};
use rmcp::{ServiceExt, transport::stdio};

use felix::Target;
use server::Server;

#[derive(Parser)]
#[command(version, about = "Felix tools for one agent, served as MCP over stdio")]
struct Cli {
    /// The run this agent works in.
    #[arg(long, env = "KBL_RUN")]
    run: String,
    /// This agent's name in the run, such as `planner` or `w1`.
    #[arg(long, env = "KBL_AGENT")]
    agent: String,
    #[arg(long, env = "KBL_TENANT", default_value = "agents")]
    tenant: String,
    #[arg(long, env = "KBL_NAMESPACE", default_value = "default")]
    namespace: String,
    /// The broker's QUIC address.
    #[arg(long, env = "KBL_FELIX_BROKER", default_value = "127.0.0.1:5000")]
    broker: String,
    /// The PEM file the broker's certificate is trusted from.
    #[arg(
        long,
        env = "KBL_FELIX_CA_FILE",
        default_value = "dev/state/broker-cert.pem"
    )]
    broker_ca: PathBuf,
    /// The name in the broker's certificate.
    #[arg(long, env = "KBL_FELIX_BROKER_NAME", default_value = "localhost")]
    broker_name: String,
    /// Where the tracker's REST API is. Without it the work tools answer that they are not set up.
    #[arg(long, env = "KBL_TRACKER_URL")]
    tracker: Option<String>,
    /// Where `/metrics` is served, on a port of its own. Off unless set. The
    /// endpoint has no authentication; see docs/metrics.md.
    #[arg(long, env = "KBL_MCP_METRICS_LISTEN")]
    metrics_listen: Option<SocketAddr>,
    /// Allows a metrics address other than loopback. Set it only when the port
    /// is reachable by the scraper alone.
    #[arg(
        long,
        env = "KBL_MCP_METRICS_ALLOW_REMOTE",
        requires = "metrics_listen"
    )]
    metrics_allow_remote: bool,
    /// How long a claim stands without an extension, in milliseconds. For tests.
    #[arg(long, env = "KBL_CLAIM_VISIBILITY_MS", hide = true)]
    claim_visibility_ms: Option<u64>,
    /// How often a held claim is extended, in milliseconds. For tests.
    #[arg(long, env = "KBL_EXTEND_EVERY_MS", hide = true)]
    extend_every_ms: Option<u64>,
}

fn main() -> Result<()> {
    kbl_core::legacy_env::adopt();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve())
}

async fn serve() -> Result<()> {
    let cli = Cli::parse();
    let exporter = kbl_metrics::install("kbl-mcp");
    if let Some(addr) = cli.metrics_listen {
        kbl_metrics::check_listen(addr, cli.metrics_allow_remote, "--metrics-allow-remote")?;
        let bound = kbl_metrics::serve(addr, exporter).await?;
        eprintln!("kbl-mcp: metrics on {bound}");
    }
    // Environment only: a flag would show the token in `ps`.
    let token = std::env::var("KBL_FELIX_TOKEN")
        .context("set KBL_FELIX_TOKEN to the agent's Felix token for the broker")?;
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
        "kbl-mcp: {} in run {}, connected to Felix",
        agent.as_str(),
        run.as_str()
    );

    // Environment only, like the Felix token.
    let tracker = match (cli.tracker, std::env::var("KBL_TRACKER_TOKEN").ok()) {
        (Some(url), Some(token)) => {
            Some(work::Http::new(&url, token).context("set up the tracker client")?)
        }
        (Some(_), None) => anyhow::bail!("set KBL_TRACKER_TOKEN to the agent's tracker token"),
        _ => None,
    };

    let timing = server::Timing {
        visibility: cli.claim_visibility_ms.map(Duration::from_millis),
        extend_every: cli.extend_every_ms.map(Duration::from_millis),
    };
    let service = Server::new(run, agent, felix, tracker, timing)
        .serve(stdio())
        .await
        .context("start the MCP server")?;
    service.waiting().await?;
    Ok(())
}
