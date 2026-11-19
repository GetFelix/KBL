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
mod supervise;
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
    let agent = parse_agent(&agent).context("--agent")?;
    let felix_settings = FelixSettings {
        tenant: cli.tenant.clone(),
        namespace: cli.namespace.clone(),
        broker: cli.broker.clone(),
        broker_ca: cli.broker_ca.clone(),
        broker_name: cli.broker_name.clone(),
    };
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
    let tracker = match (cli.tracker.clone(), std::env::var("KBL_TRACKER_TOKEN").ok()) {
        (Some(url), Some(token)) => {
            Some(work::Http::new(&url, token).context("set up the tracker client")?)
        }
        (Some(_), None) => anyhow::bail!("set KBL_TRACKER_TOKEN to the agent's tracker token"),
        _ => None,
    };

    let pinned = match std::env::var("KBL_WORK_FILTER") {
        Ok(f) if !f.is_empty() => {
            let v: serde_json::Value = serde_json::from_str(&f).context("KBL_WORK_FILTER")?;
            anyhow::ensure!(v.is_object(), "KBL_WORK_FILTER must be a JSON object");
            v
        }
        _ => serde_json::json!({}),
    };
    let supervise = match std::env::var("KBL_SUPERVISE_CONFIG") {
        Ok(path) if !path.is_empty() => {
            let (Some(api), Some(url)) = (tracker.clone(), cli.tracker.clone()) else {
                anyhow::bail!("KBL_SUPERVISE_CONFIG needs KBL_TRACKER_URL and KBL_TRACKER_TOKEN");
            };
            let config = supervise::Config::load(std::path::Path::new(&path))?;
            let base = worker_base(&felix_settings, &run, &url, config.share_felix_token)?;
            let sup = supervise::Supervisor::new(
                api,
                agent.clone(),
                config,
                base,
                Box::new(|name| std::env::var(name).ok()),
            )?;
            sup.spawn_top_up();
            Some(sup)
        }
        _ => None,
    };

    let timing = server::Timing {
        visibility: cli.claim_visibility_ms.map(Duration::from_millis),
        extend_every: cli.extend_every_ms.map(Duration::from_millis),
    };
    let service = Server::new(run, agent, felix, tracker, timing, pinned, supervise)
        .serve(stdio())
        .await
        .context("start the MCP server")?;
    service.waiting().await?;
    Ok(())
}

/// The Felix settings a worker's `kbl-mcp` is started with.
struct FelixSettings {
    tenant: String,
    namespace: String,
    broker: String,
    broker_ca: PathBuf,
    broker_name: String,
}

/// What every worker's MCP config sets besides its name, token and filter.
/// Felix credentials go by reference (an ID token file or command); a fixed
/// Felix token is copied only when the config says to share it.
fn worker_base(
    f: &FelixSettings,
    run: &RunId,
    tracker_url: &str,
    share_felix_token: bool,
) -> Result<supervise::Base> {
    let get = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let abs = |p: &str| {
        let p = PathBuf::from(p);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir().map_or(p.clone(), |d| d.join(&p))
        }
    };
    let mut env = std::collections::BTreeMap::new();
    env.insert("KBL_RUN".to_owned(), run.as_str().to_owned());
    env.insert("KBL_TENANT".to_owned(), f.tenant.clone());
    env.insert("KBL_NAMESPACE".to_owned(), f.namespace.clone());
    env.insert("KBL_FELIX_BROKER".to_owned(), f.broker.clone());
    env.insert(
        "KBL_FELIX_CA_FILE".to_owned(),
        abs(&f.broker_ca.to_string_lossy())
            .to_string_lossy()
            .into_owned(),
    );
    env.insert("KBL_FELIX_BROKER_NAME".to_owned(), f.broker_name.clone());
    env.insert("KBL_TRACKER_URL".to_owned(), tracker_url.to_owned());
    for name in [
        "KBL_FELIX_ID_TOKEN_COMMAND",
        "KBL_FELIX_CONTROL_PLANE",
        "KBL_FELIX_TOKEN_MAX_AGE_MS",
        "KBL_CLAIM_VISIBILITY_MS",
        "KBL_EXTEND_EVERY_MS",
    ] {
        if let Some(v) = get(name) {
            env.insert(name.to_owned(), v);
        }
    }
    if let Some(file) = get("KBL_FELIX_ID_TOKEN_FILE") {
        env.insert(
            "KBL_FELIX_ID_TOKEN_FILE".to_owned(),
            abs(&file).to_string_lossy().into_owned(),
        );
    }
    if let Some(token) = get("KBL_FELIX_TOKEN") {
        anyhow::ensure!(
            share_felix_token,
            "this kbl-mcp has a fixed KBL_FELIX_TOKEN: set share_felix_token = true in the supervise config to give workers a copy, or renew from an ID token"
        );
        env.insert("KBL_FELIX_TOKEN".to_owned(), token);
    }
    Ok(supervise::Base {
        env,
        tracker_url: tracker_url.to_owned(),
    })
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

/// `--agent`: a plain agent name, or the `<supervisor>-<suffix>` name of a
/// delegated worker, which is what `kbl-supervise` puts in `KBL_AGENT`.
fn parse_agent(s: &str) -> Result<AgentName, kbl_core::names::InvalidName> {
    AgentName::agent_or_worker(s)
}

#[cfg(test)]
mod agent_arg_tests {
    use super::parse_agent;

    #[test]
    fn accepts_agent_and_worker_names() {
        assert!(parse_agent("w1").is_ok());
        assert_eq!(parse_agent("sup-w1").unwrap().as_str(), "sup-w1");
        for bad in ["a-b-c", "plan", "-x", "x-", ""] {
            assert!(parse_agent(bad).is_err(), "{bad}");
        }
    }
}
