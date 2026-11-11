//! `agents-tool`: sets up what a Felix Agents run needs on Felix.
//!
//! ```text
//! agents-tool run create demo --goal "Review PR 12" --budget-usd 5 --workers 4
//! ```

mod felix;
mod run;

use std::time::{SystemTime, UNIX_EPOCH};

use agents_core::codec::Codec;
use agents_core::names::{Resources, RunId, WORKERS_GROUP};
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use felix::{Outcome, Session, Target};
use run::NewRun;

#[derive(Parser)]
#[command(version, about = "Set up Felix for a Felix Agents run")]
struct Cli {
    #[command(flatten)]
    target: TargetArgs,
    #[command(subcommand)]
    command: Command,
}

/// Where Felix is. The defaults match the development stack in `dev/`.
#[derive(Args)]
struct TargetArgs {
    #[arg(long, env = "AGENTS_TENANT", default_value = "agents")]
    tenant: String,
    #[arg(long, env = "AGENTS_NAMESPACE", default_value = "default")]
    namespace: String,
    #[arg(
        long,
        env = "AGENTS_FELIX_CONTROL_PLANE",
        default_value = "http://127.0.0.1:8443"
    )]
    control_plane: String,
    /// The development IdP, which signs `agents-admin` in.
    #[arg(long, env = "AGENTS_IDP", default_value = "http://127.0.0.1:9400")]
    idp: String,
    /// An ID token for the tenant, used instead of asking the development IdP.
    #[arg(long, env = "AGENTS_ID_TOKEN", hide_env_values = true)]
    id_token: Option<String>,
    /// The broker's QUIC address.
    #[arg(long, env = "AGENTS_FELIX_BROKER", default_value = "127.0.0.1:5000")]
    broker: String,
    /// The PEM file the broker's certificate is trusted from.
    #[arg(
        long,
        env = "AGENTS_FELIX_CA_FILE",
        default_value = "dev/state/broker-cert.pem"
    )]
    broker_ca: String,
    /// The name in the broker's certificate.
    #[arg(long, env = "AGENTS_FELIX_BROKER_NAME", default_value = "localhost")]
    broker_name: String,
}

#[derive(Subcommand)]
enum Command {
    /// Work with runs.
    #[command(subcommand)]
    Run(RunCommand),
}

#[derive(Subcommand)]
enum RunCommand {
    /// Create a run's streams, caches and `workers` group, and log `run_started`.
    Create(CreateArgs),
    /// Print a run's log, one JSON record per line.
    Log {
        /// The run's id.
        run: String,
    },
}

#[derive(Args)]
struct CreateArgs {
    /// The run's id: 1 to 64 letters, digits, `-` or `_`.
    run: String,
    #[arg(long)]
    goal: String,
    /// The most the run may spend, in dollars, such as 5 or 0.25.
    #[arg(long = "budget-usd", value_parser = run::parse_budget, value_name = "DOLLARS")]
    budget_micros: u64,
    /// How many workers the run lists, named w1, w2 and so on.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..=run::MAX_WORKERS as i64))]
    workers: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let target = Target {
        tenant: cli.target.tenant,
        namespace: cli.target.namespace,
        control_plane: cli.target.control_plane.trim_end_matches('/').to_owned(),
        idp: cli.target.idp.trim_end_matches('/').to_owned(),
        id_token: cli.target.id_token,
        broker: cli.target.broker,
        broker_ca: cli.target.broker_ca,
        broker_name: cli.target.broker_name,
    };
    match cli.command {
        Command::Run(RunCommand::Create(args)) => create(target, args).await,
        Command::Run(RunCommand::Log { run }) => log(target, &run).await,
    }
}

async fn create(target: Target, args: CreateArgs) -> Result<()> {
    let run = NewRun {
        run: RunId::new(&args.run).map_err(|e| anyhow::anyhow!("{e}"))?,
        goal: args.goal,
        budget_micros: args.budget_micros,
        workers: args.workers,
    };
    let resources = run.resources();
    let started = run.started(now_ms()).map_err(|e| anyhow::anyhow!("{e}"))?;

    let session = Session::sign_in(target).await?;
    // Connect first: creating the resources spends the run id, so every
    // failure we can see coming has to happen before it.
    let broker = session.broker().await?;
    match session.create_resources(&run::batch(&resources)).await? {
        Outcome::AllNew => {}
        Outcome::NoneNew => bail!(
            "run {} already exists. Pick another id; a run is never created twice.",
            args.run
        ),
        Outcome::Mixed => bail!(
            "some of run {}'s resources already existed, so it is now incomplete. Pick another id.",
            args.run
        ),
    }
    broker
        .create_group(&resources.tasks(), WORKERS_GROUP)
        .await?;
    let offset = broker
        .publish(&resources.log(), started.encode())
        .await
        .context("the run's resources exist, but run_started may not have been written")?;

    println!(
        "created run {} ({} resources)",
        args.run,
        resources.all().len()
    );
    println!(
        "  log      {} (run_started at offset {offset})",
        resources.log()
    );
    println!("  tasks    {} (group {WORKERS_GROUP})", resources.tasks());
    Ok(())
}

async fn log(target: Target, run: &str) -> Result<()> {
    let run = RunId::new(run).map_err(|e| anyhow::anyhow!("{e}"))?;
    let broker = Session::sign_in(target).await?.broker().await?;
    for payload in broker.read_all(&Resources::new(run).log()).await? {
        println!("{}", String::from_utf8_lossy(&payload));
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests;
