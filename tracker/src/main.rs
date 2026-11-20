//! `kbl-tracker`: the work tracker. It folds `work.events`, publishes ready
//! items to `work.ready.p0` to `p3`, and serves the claim path to agents over
//! REST. It makes no model calls. See docs/work-tracker.md.

mod config;
mod felix;
mod http;
mod org;
mod teams;
mod tracker;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use config::{Grants, Tokens};
use felix::{Kind, Target};
use kbl_core::names::Key;
use kbl_core::org::{ORG_NAMESPACE, OrgRecord, TeamName};
use kbl_core::org_fold::OrgState;
use org::Org;
use teams::{OrgSide, Starter, Team, Teams};
use tracker::{Settings, Tracker};

#[derive(Parser)]
#[command(version, about = "The work tracker for Felix Agents")]
struct Cli {
    /// Where the REST API listens.
    #[arg(long, env = "KBL_TRACKER_LISTEN", default_value = "127.0.0.1:8790")]
    listen: SocketAddr,
    /// Allows a listen address other than loopback. Set it only when a proxy in
    /// front terminates TLS: the tracker serves plain HTTP, and every call
    /// carries a bearer token.
    #[arg(long, env = "KBL_TRACKER_BEHIND_TLS_PROXY")]
    behind_tls_proxy: bool,
    /// Where `/metrics` is served, on a port of its own. Off unless set. The
    /// endpoint has no authentication; see docs/metrics.md.
    #[arg(long, env = "KBL_TRACKER_METRICS_LISTEN")]
    metrics_listen: Option<SocketAddr>,
    /// Allows a metrics address other than loopback. Set it only when the port
    /// is reachable by the scraper alone.
    #[arg(
        long,
        env = "KBL_TRACKER_METRICS_ALLOW_REMOTE",
        requires = "metrics_listen"
    )]
    metrics_allow_remote: bool,
    /// JSON file mapping tokens to agent names and people:
    /// {"agents": {"<token>": "w1"}, "people": {"<token>": "gabriel"}}.
    #[arg(long, env = "KBL_TRACKER_TOKENS_FILE")]
    tokens_file: PathBuf,
    #[arg(long, env = "KBL_TENANT", default_value = "agents")]
    tenant: String,
    /// The team's Felix namespace, which is also the team's name. With teams
    /// on, the team the token file's agents and ingest workers belong to.
    #[arg(long, env = "KBL_NAMESPACE", default_value = "default")]
    namespace: String,
    /// Turns teams on: the namespace that holds `org.events`. It must be `kbl`.
    #[arg(long, env = "KBL_ORG_NAMESPACE")]
    org_namespace: Option<String>,
    /// Calls in flight per team. A call over it answers 503 with `team_busy`.
    #[arg(long, env = "KBL_TEAM_INFLIGHT", default_value_t = teams::DEFAULT_INFLIGHT)]
    team_inflight: usize,
    #[arg(
        long,
        env = "KBL_FELIX_CONTROL_PLANE",
        default_value = "http://127.0.0.1:8443"
    )]
    control_plane: String,
    /// The development IdP, which signs the tracker in.
    #[arg(long, env = "KBL_IDP", default_value = "http://127.0.0.1:9400")]
    idp: String,
    /// An ID token for the tenant, used instead of asking the development IdP.
    #[arg(long, env = "KBL_ID_TOKEN", hide_env_values = true)]
    id_token: Option<String>,
    #[arg(long, env = "KBL_FELIX_BROKER", default_value = "127.0.0.1:5000")]
    broker: String,
    #[arg(
        long,
        env = "KBL_FELIX_CA_FILE",
        default_value = "dev/state/broker-cert.pem"
    )]
    broker_ca: PathBuf,
    #[arg(long, env = "KBL_FELIX_BROKER_NAME", default_value = "localhost")]
    broker_name: String,
    /// How long a claim stands without an extension, in milliseconds. For tests.
    #[arg(
        long,
        env = "KBL_CLAIM_VISIBILITY_MS",
        default_value_t = 90_000,
        hide = true
    )]
    claim_visibility_ms: u64,
    /// How long the writer lease stands without a renewal, in milliseconds.
    #[arg(
        long,
        env = "KBL_TRACKER_LEASE_TTL_MS",
        default_value_t = 15_000,
        hide = true
    )]
    lease_ttl_ms: u64,
    /// The reviewer tiers, comma separated, at most four. The first reads a pull request
    /// first, the last gives the final review.
    #[arg(
        long,
        env = "KBL_REVIEW_TIERS",
        value_delimiter = ',',
        default_value = "fast,deep"
    )]
    review_tiers: Vec<String>,
    /// Tries an agent gets at a failed check on one pull request, each a
    /// `fix_check` item, before the failure goes to a person. Zero turns
    /// them off.
    #[arg(long, env = "KBL_AUTO_FIX_TRIES", default_value_t = 1)]
    auto_fix_tries: u8,
    /// The least each claim is charged against its root's cost budget, in
    /// millionths of a dollar, whatever the agent reports.
    #[arg(long, env = "KBL_COST_FLOOR_MICROS", default_value_t = 100_000)]
    cost_floor_micros: u64,
    /// Agent pull requests in In review at which agents stop taking new fixes.
    /// Triage, answers and reviews carry on. Zero turns the limit off. The default
    /// is a guess with no measurement behind it (docs/dashboard-research.md).
    #[arg(long, env = "KBL_REVIEW_LIMIT", default_value_t = 3)]
    review_limit: usize,
    /// A folder for fold snapshots, one subfolder per team. A restart then folds
    /// only the records since the newest instead of all of work.events. Unset keeps none.
    #[arg(long, env = "KBL_TRACKER_SNAPSHOT_DIR")]
    snapshot_dir: Option<std::path::PathBuf>,
    /// Records between snapshots, saved by a rebuild and by the running tracker. 0 turns saving off.
    #[arg(
        long,
        env = "KBL_TRACKER_SNAPSHOT_EVERY",
        default_value_t = kbl_core::work_fold::SNAPSHOT_EVERY
    )]
    snapshot_every: u64,
    /// Seconds an agent assignee may make no call before the tracker drops it
    /// and its Ready items go to the whole team.
    #[arg(long, env = "KBL_ASSIGNEE_AWAY_SECS", default_value_t = 600)]
    assignee_away_secs: u64,
    /// Live delegated workers one supervisor's token may have.
    #[arg(long, env = "KBL_MAX_WORKERS", default_value_t = 8)]
    max_workers: usize,
    /// Live delegated workers a team may have, over all its supervisors.
    #[arg(long, env = "KBL_MAX_TEAM_WORKERS", default_value_t = 32)]
    max_team_workers: usize,
    /// Exit after polling a ready item and before logging its claim. For the dev-stack check.
    #[arg(long, env = "KBL_TRACKER_CRASH_AFTER_POLL", hide = true)]
    crash_after_poll: bool,
    /// Exit after logging a terminal record and before answering. For the dev-stack check.
    #[arg(long, env = "KBL_TRACKER_CRASH_AFTER_DONE", hide = true)]
    crash_after_done: bool,
}

fn main() -> Result<()> {
    kbl_core::legacy_env::adopt();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve())
}

/// Plain HTTP stays on loopback unless a TLS proxy is said to be in front,
/// so tokens never cross a network in the clear by accident.
fn check_listen(addr: SocketAddr, behind_tls_proxy: bool) -> Result<()> {
    anyhow::ensure!(
        behind_tls_proxy || addr.ip().to_canonical().is_loopback(),
        "the tracker serves plain HTTP, so it listens only on loopback; {addr} is not. \
         Put a TLS proxy in front and set KBL_TRACKER_BEHIND_TLS_PROXY=true (--behind-tls-proxy)"
    );
    Ok(())
}

async fn serve() -> Result<()> {
    let cli = Cli::parse();
    check_listen(cli.listen, cli.behind_tls_proxy)?;
    // Installed before any team starts, so startup is recorded too.
    let exporter = kbl_metrics::install("kbl-tracker");
    // Held to the end of `serve`, which flushes the last spans.
    let _traces = kbl_trace::init("kbl-tracker");
    if let Some(addr) = cli.metrics_listen {
        kbl_metrics::check_listen(addr, cli.metrics_allow_remote, "--metrics-allow-remote")?;
        let bound = kbl_metrics::serve(addr, exporter).await?;
        eprintln!("kbl-tracker: metrics on {bound}");
    }
    let team = TeamName::new(cli.namespace.clone()).map_err(|e| {
        anyhow::anyhow!(
            "KBL_NAMESPACE {:?} cannot name a team: it must follow the key rule and not be `kbl`",
            e.0
        )
    })?;
    if let Some(ns) = &cli.org_namespace {
        anyhow::ensure!(
            ns == ORG_NAMESPACE,
            "KBL_ORG_NAMESPACE is {ns:?}; the org namespace is `{ORG_NAMESPACE}`"
        );
    }
    let tokens = Tokens::load(&cli.tokens_file)?;
    let ttl = Duration::from_millis(cli.lease_ttl_ms);
    let start = starter(&cli, team.clone(), tokens.grants());
    let teams = match cli.org_namespace.is_some() {
        false => {
            // Without teams a team that cannot start stops the process, as before.
            let tracker = start(team.clone()).await?;
            Teams::new(vec![Team::new(team, tracker, tokens, cli.team_inflight)])?
        }
        true => with_org(&cli, team, tokens, start, ttl).await?,
    };
    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("listen on {}", cli.listen))?;
    eprintln!("kbl-tracker: listening on {}", listener.local_addr()?);
    let teams = Arc::new(teams);
    let maintain = teams.clone();
    tokio::select! {
        served = axum::serve(listener, http::router(teams)) => served.context("serve"),
        () = async move { maintain.maintain().await } => {
            anyhow::bail!("no team holds its writer lease, the org lease was lost, or a team's maintenance failed")
        }
    }
}

/// Where Felix is for one namespace.
fn target(cli: &Cli, namespace: &str) -> Target {
    Target {
        tenant: cli.tenant.clone(),
        namespace: namespace.to_owned(),
        control_plane: cli.control_plane.trim_end_matches('/').to_owned(),
        idp: cli.idp.trim_end_matches('/').to_owned(),
        id_token: cli.id_token.clone(),
        broker: cli.broker.clone(),
        broker_ca: cli.broker_ca.clone(),
        broker_name: cli.broker_name.clone(),
    }
}

/// Starts a team's tracker. The token file's grants belong to `first`, the
/// team its agents are in.
fn starter(cli: &Cli, first: TeamName, grants: Grants) -> Starter<felix::Felix> {
    let ttl = Duration::from_millis(cli.lease_ttl_ms);
    let visibility = Duration::from_millis(cli.claim_visibility_ms);
    let (crash_after_poll, crash_after_done) = (cli.crash_after_poll, cli.crash_after_done);
    let review_tiers = cli.review_tiers.clone();
    let auto_fix_tries = cli.auto_fix_tries;
    // One floor for every team; each team's tracker charges it against its own fold.
    let cost_floor_micros = cli.cost_floor_micros;
    let review_limit = cli.review_limit;
    let snapshot_dir = cli.snapshot_dir.clone();
    let snapshot_every = cli.snapshot_every;
    let assignee_away = Duration::from_secs(cli.assignee_away_secs);
    let (max_workers, max_team_workers) = (cli.max_workers, cli.max_team_workers);
    let base = target(cli, "");
    Arc::new(move |team: TeamName| {
        let mut target = base.clone();
        target.namespace = team.as_str().to_owned();
        let settings = Settings {
            grants: if team == first {
                grants.clone()
            } else {
                Grants::default()
            },
            visibility,
            lease_ttl: ttl,
            // Long enough for a killed tracker's lease to run out.
            lease_wait: ttl * 2,
            crash_after_poll,
            crash_after_done,
            review_tiers: review_tiers.clone(),
            auto_fix_tries,
            cost_floor_micros,
            team: team.as_str().to_owned(),
            review_limit,
            snapshot_dir: snapshot_dir.clone(),
            snapshot_every,
            assignee_away,
            max_workers,
            max_team_workers,
            fresh_format: kbl_core::work::FORMAT,
        };
        Box::pin(async move {
            let felix = felix::connect(target, Kind::Team).await?;
            Tracker::start(felix, settings).await
        })
    })
}

/// Takes the org lease, adopts `first` when `org.events` is empty, and starts
/// every team the log lists. A team that cannot start answers 503 and is
/// retried in the background; the others serve.
async fn with_org(
    cli: &Cli,
    first: TeamName,
    tokens: Tokens,
    start: Starter<felix::Felix>,
    ttl: Duration,
) -> Result<Teams<felix::Felix>> {
    let felix = felix::connect(target(cli, ORG_NAMESPACE), Kind::Org).await?;
    let org = Arc::new(Org::start(felix, ttl, ttl * 2).await?);
    if org.state().await.teams().next().is_none() {
        let adopt = OrgRecord::TeamCreated {
            team: first.clone(),
            display_name: None,
            adopted: true,
        };
        org.append(Key::new("tracker").expect("a valid key"), adopt)
            .await
            .map_err(|e| anyhow::anyhow!("could not adopt team {first}: {e:?}"))?;
        eprintln!("kbl-tracker: adopted {first} as the org's first team");
    }
    let names: Vec<TeamName> = org.state().await.teams().map(|(t, _)| t.clone()).collect();
    anyhow::ensure!(
        names.contains(&first),
        "KBL_NAMESPACE {first} is not a team on org.events, so the token file's agents would have no team"
    );
    file_agents_not_minted(&org.state().await, &first, &tokens)?;
    let admins = tokens.people_only();
    let mut teams = Vec::new();
    for name in names {
        let own = if name == first {
            tokens.clone()
        } else {
            admins.people_only()
        };
        teams.push(match start(name.clone()).await {
            Ok(tracker) => Team::new(name, tracker, own, cli.team_inflight),
            Err(e) => {
                eprintln!("kbl-tracker: team {name} did not start: {e:#}");
                Team::unavailable(name, own, cli.team_inflight)
            }
        });
    }
    Teams::with_org(
        teams,
        Some(OrgSide {
            org,
            start,
            admins,
            inflight: cli.team_inflight,
        }),
    )
}

/// Refuses a token file that names an agent `agent add` minted and nobody
/// revoked: two live tokens would answer to one name, and revoking the minted
/// one would also stop the file's agent from claiming.
fn file_agents_not_minted(state: &OrgState, team: &TeamName, tokens: &Tokens) -> Result<()> {
    for name in tokens.agent_names() {
        let key = Key::new(name.as_str()).expect("an agent name is a key");
        if state.agent(team, &key).is_some_and(|a| !a.revoked) {
            anyhow::bail!(
                "the token file names agent {name} of team {team}, which `kbl-tool agent add` also \
                 minted and has not revoked: take it out of the token file, start, run `kbl-tool agent \
                 revoke`, then put it back"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
