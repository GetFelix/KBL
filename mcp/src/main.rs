//! `kbl-mcp`: the tools one agent in a Felix Agents run is given, served as
//! MCP over stdio. The agent's token comes from the environment, or is renewed
//! from an ID token (see `credential.rs`), so the agent can do on Felix what its
//! token allows and nothing else.
//!
//! `kbl-mcp person` is the person-side server: a person's card actions, with
//! a person's token, in a process of its own. See `person.rs`.
//!
//! Stdout belongs to the protocol. Anything meant for a person goes to stderr.

mod credential;
mod felix;
mod keepalive;
mod metrics;
mod person;
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
#[command(
    version,
    about = "Felix tools for one agent, served as MCP over stdio",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// The run this agent works in.
    #[arg(long, env = "KBL_RUN", required = true)]
    run: Option<String>,
    /// This agent's name in the run, such as `planner` or `w1`.
    #[arg(long, env = "KBL_AGENT", required = true)]
    agent: Option<String>,
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

#[derive(clap::Subcommand)]
enum Command {
    /// Serve a person's card actions and My queue, with a person's token.
    Person(PersonArgs),
}

#[derive(clap::Args)]
struct PersonArgs {
    /// Where the tracker's REST API is.
    #[arg(long, env = "KBL_TRACKER_URL")]
    tracker: String,
    /// The team to act in. Needed when the token is in more than one.
    #[arg(long, env = "KBL_TEAM")]
    team: Option<String>,
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
    if let Some(Command::Person(args)) = cli.command {
        return serve_person(args).await;
    }
    let exporter = kbl_metrics::install("kbl-mcp");
    // Held to the end of `serve`, which flushes the last spans.
    let _traces = kbl_trace::init("kbl-mcp");
    if let Some(addr) = cli.metrics_listen {
        kbl_metrics::check_listen(addr, cli.metrics_allow_remote, "--metrics-allow-remote")?;
        let bound = kbl_metrics::serve(addr, exporter).await?;
        eprintln!("kbl-mcp: metrics on {bound}");
    }
    // Environment and files only: a flag would show a secret in `ps`.
    let auth = credential::Auth::from_env(|name| std::env::var(name).ok(), &cli.tenant)?;
    let (Some(run), Some(agent)) = (cli.run, cli.agent) else {
        anyhow::bail!("--run and --agent are needed");
    };
    let run = RunId::new(run).context("--run")?;
    let agent = AgentName::new(agent).context("--agent")?;
    let felix = felix::connect(Target {
        tenant: cli.tenant,
        namespace: cli.namespace,
        broker: cli.broker,
        broker_ca: cli.broker_ca,
        broker_name: cli.broker_name,
        auth,
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

/// The person server. Its token is `KBL_PERSON_TOKEN`, a name the agent server
/// never reads, so an agent's environment cannot be mistaken for it.
async fn serve_person(args: PersonArgs) -> Result<()> {
    let token = std::env::var("KBL_PERSON_TOKEN")
        .context("set KBL_PERSON_TOKEN to a person's tracker token")?;
    let team = args
        .team
        .map(kbl_core::org::TeamName::new)
        .transpose()
        .context("--team")?;
    let api = work::Http::new(&args.tracker, token).context("set up the tracker client")?;
    let person = person::Person::connect(api, team)
        .await
        .map_err(anyhow::Error::msg)?;
    eprintln!("kbl-mcp: person tools, {} tools", person.tools().len());
    let service = person
        .serve(stdio())
        .await
        .context("start the MCP server")?;
    service.waiting().await?;
    Ok(())
}
