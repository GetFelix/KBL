//! `agents-tracker`: the work tracker. It folds `work.events`, publishes ready
//! items to `work.ready.p0` to `p3`, and serves the claim path to agents over
//! REST. It makes no model calls. See docs/work-tracker.md.

mod config;
mod felix;
mod http;
mod tracker;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use config::Tokens;
use felix::Target;
use tracker::{Settings, Tracker};

#[derive(Parser)]
#[command(version, about = "The work tracker for Felix Agents")]
struct Cli {
    /// Where the REST API listens.
    #[arg(long, env = "AGENTS_TRACKER_LISTEN", default_value = "127.0.0.1:8790")]
    listen: SocketAddr,
    /// JSON file mapping tokens to agent names and people:
    /// {"agents": {"<token>": "w1"}, "people": {"<token>": "gabriel"}}.
    #[arg(long, env = "AGENTS_TRACKER_TOKENS_FILE")]
    tokens_file: PathBuf,
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
    /// The development IdP, which signs the tracker in.
    #[arg(long, env = "AGENTS_IDP", default_value = "http://127.0.0.1:9400")]
    idp: String,
    /// An ID token for the tenant, used instead of asking the development IdP.
    #[arg(long, env = "AGENTS_ID_TOKEN", hide_env_values = true)]
    id_token: Option<String>,
    #[arg(long, env = "AGENTS_FELIX_BROKER", default_value = "127.0.0.1:5000")]
    broker: String,
    #[arg(
        long,
        env = "AGENTS_FELIX_CA_FILE",
        default_value = "dev/state/broker-cert.pem"
    )]
    broker_ca: PathBuf,
    #[arg(long, env = "AGENTS_FELIX_BROKER_NAME", default_value = "localhost")]
    broker_name: String,
    /// How long a claim stands without an extension, in milliseconds. For tests.
    #[arg(
        long,
        env = "AGENTS_CLAIM_VISIBILITY_MS",
        default_value_t = 90_000,
        hide = true
    )]
    claim_visibility_ms: u64,
    /// How long the writer lease stands without a renewal, in milliseconds.
    #[arg(
        long,
        env = "AGENTS_TRACKER_LEASE_TTL_MS",
        default_value_t = 15_000,
        hide = true
    )]
    lease_ttl_ms: u64,
    /// The reviewer tiers, comma separated, at most four. The first reads a pull request
    /// first, the last gives the final review.
    #[arg(
        long,
        env = "AGENTS_REVIEW_TIERS",
        value_delimiter = ',',
        default_value = "fast,deep"
    )]
    review_tiers: Vec<String>,
    /// Exit after polling a ready item and before logging its claim. For the dev-stack check.
    #[arg(long, env = "AGENTS_TRACKER_CRASH_AFTER_POLL", hide = true)]
    crash_after_poll: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let tokens = Tokens::load(&cli.tokens_file)?;
    let grants = tokens.grants();
    let felix = felix::connect(Target {
        tenant: cli.tenant,
        namespace: cli.namespace,
        control_plane: cli.control_plane.trim_end_matches('/').to_owned(),
        idp: cli.idp.trim_end_matches('/').to_owned(),
        id_token: cli.id_token,
        broker: cli.broker,
        broker_ca: cli.broker_ca,
        broker_name: cli.broker_name,
    })
    .await?;
    let ttl = Duration::from_millis(cli.lease_ttl_ms);
    let tracker = Tracker::start(
        felix,
        Settings {
            grants,
            visibility: Duration::from_millis(cli.claim_visibility_ms),
            lease_ttl: ttl,
            // Long enough for a killed tracker's lease to run out.
            lease_wait: ttl * 2,
            crash_after_poll: cli.crash_after_poll,
            review_tiers: cli.review_tiers,
        },
    )
    .await?;
    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("listen on {}", cli.listen))?;
    eprintln!("agents-tracker: listening on {}", listener.local_addr()?);
    let maintain = tracker.clone();
    tokio::select! {
        served = axum::serve(listener, http::router(tracker, tokens)) => served.context("serve"),
        () = async move { maintain.maintain().await } => {
            anyhow::bail!("the writer lease was lost")
        }
    }
}
