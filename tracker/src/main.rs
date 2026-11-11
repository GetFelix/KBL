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
use tracker::Tracker;

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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let tokens = Tokens::load(&cli.tokens_file)?;
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
    let tracker = Tracker::start(felix, Duration::from_millis(cli.claim_visibility_ms)).await?;
    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("listen on {}", cli.listen))?;
    eprintln!("agents-tracker: listening on {}", listener.local_addr()?);
    axum::serve(listener, http::router(tracker, tokens))
        .await
        .context("serve")
}
