//! `kbl-ingest`: pulls issues from upstream trackers into the work tracker
//! and keeps them in step. It writes only through the tracker's REST API, under
//! its own token, and makes no model calls. See docs/work-tracker.md "Sources".

mod ado;
mod backoff;
mod budget;
mod check;
mod config;
mod github;
mod gitlab;
mod jira;
mod lease;
mod metrics;
mod rules;
mod source;
mod state;
mod sync;
mod tracker_api;
mod webhook;

#[cfg(test)]
mod testing;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;

use config::Config;
use github::Github;
use github::app::{self, AppAuth};
use github::transport::{Reqwest, Token};
use source::{Source, SourceError};
use sync::{Ingest, Settings};

#[derive(Parser)]
#[command(
    version,
    about = "The upstream issue ingest worker for Felix Agents",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// JSON file with the tracker, GitHub and intake rules. See docs/work-tracker.md.
    #[arg(long, env = "KBL_INGEST_CONFIG", required = true)]
    config: Option<PathBuf>,
    /// Where the optional webhook endpoint listens. Webhooks only wake the poll.
    #[arg(
        long,
        env = "KBL_INGEST_WEBHOOK_LISTEN",
        requires = "webhook_secret_env"
    )]
    webhook_listen: Option<SocketAddr>,
    /// Allows a webhook address other than loopback. Set it only when a proxy
    /// in front terminates TLS: the endpoint serves plain HTTP, and a delivery
    /// carries the issue's text.
    #[arg(
        long,
        env = "KBL_INGEST_WEBHOOK_BEHIND_TLS_PROXY",
        requires = "webhook_listen"
    )]
    webhook_behind_tls_proxy: bool,
    /// Where `/metrics` is served, on a port of its own. Off unless set. The
    /// endpoint has no authentication; see docs/metrics.md.
    #[arg(long, env = "KBL_INGEST_METRICS_LISTEN")]
    metrics_listen: Option<SocketAddr>,
    /// Allows a metrics address other than loopback. Set it only when the port
    /// is reachable by the scraper alone.
    #[arg(
        long,
        env = "KBL_INGEST_METRICS_ALLOW_REMOTE",
        requires = "metrics_listen"
    )]
    metrics_allow_remote: bool,
    /// The environment variable holding the webhook secret.
    #[arg(long, env = "KBL_INGEST_WEBHOOK_SECRET_ENV")]
    webhook_secret_env: Option<String>,
    /// The least seconds between two webhook wakes of the poll.
    #[arg(long, env = "KBL_INGEST_WEBHOOK_MIN_GAP_SECONDS", default_value_t = 10)]
    webhook_min_gap_seconds: u64,
    /// Poll once and exit.
    #[arg(long)]
    once: bool,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Validate a config, test its credentials and show what each rule would
    /// import. Reads only: it writes nothing to the tracker or the source.
    Check(check::Args),
}

/// Plain HTTP stays on loopback unless a TLS proxy is said to be in front, so
/// a private repo's issue text never crosses a network in the clear by accident.
fn check_listen(addr: SocketAddr, behind_tls_proxy: bool) -> Result<()> {
    anyhow::ensure!(
        behind_tls_proxy || addr.ip().to_canonical().is_loopback(),
        "the webhook serves plain HTTP, so it listens only on loopback; {addr} is not. \
         Put a TLS proxy in front and set KBL_INGEST_WEBHOOK_BEHIND_TLS_PROXY=true \
         (--webhook-behind-tls-proxy)"
    );
    Ok(())
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
    if let Some(Command::Check(args)) = &cli.command {
        return if check::run(args).await? {
            Ok(())
        } else {
            std::process::exit(1)
        };
    }
    let config = cli.config.as_deref().context("--config is required")?;
    let cfg = Config::load(config)?;
    let exporter = kbl_metrics::install("kbl-ingest");
    // Held to the end of `serve`, which flushes the last spans.
    let _traces = kbl_trace::init("kbl-ingest");
    if let Some(addr) = cli.metrics_listen {
        kbl_metrics::check_listen(addr, cli.metrics_allow_remote, "--metrics-allow-remote")?;
        let bound = kbl_metrics::serve(addr, exporter).await?;
        eprintln!("kbl-ingest: metrics on {bound}");
    }
    let token = std::env::var(&cfg.tracker.token_env)
        .with_context(|| format!("{} is not set", cfg.tracker.token_env))?;
    let tracker = tracker_api::Http::new(&cfg.tracker.url, token.clone())?;
    let lease_api = Arc::new(tracker_api::Http::new(&cfg.tracker.url, token)?);
    let vendor = cfg.vendor();
    let lessee = match &cfg.tracker.group {
        None => lease::Lessee::file_managed(vendor),
        Some(_) => lease::Lessee::waiting(vendor, cfg.repos()),
    };
    let lanes = (tracker, lease_api, lessee.clone());
    let budget = budget::Budget::new(
        budget::Vendor::named(vendor),
        cfg.budget_per_hour,
        cfg.budget_share,
    );
    lessee.attach(budget.clone());
    match (&cfg.github, &cfg.jira, &cfg.ado, &cfg.gitlab) {
        (Some(g), None, None, None) => {
            let github = github_source(g, &lessee, &budget).await?;
            drive(github, lanes, budget, cfg, cli).await
        }
        (None, Some(j), None, None) => {
            let jira = jira_source(j, &cfg, &lessee, &budget).await?;
            drive(jira, lanes, budget, cfg, cli).await
        }
        (None, None, Some(a), None) => {
            let ado = ado_source(a, &cfg, &lessee, &budget).await?;
            drive(ado, lanes, budget, cfg, cli).await
        }
        (None, None, None, Some(g)) => {
            let gitlab = gitlab_source(g, &cfg, &lessee, &budget).await?;
            drive(gitlab, lanes, budget, cfg, cli).await
        }
        _ => unreachable!("the config checks for exactly one source"),
    }
}

/// A token_file wins over an app, and an app over a token_env.
pub(crate) fn github_token(
    gh: &config::GithubConfig,
    budget: &Arc<budget::Budget>,
) -> Result<Token> {
    Ok(match (&gh.token_file, &gh.app, &gh.token_env) {
        (Some(f), _, _) => Token::File(f.clone()),
        (None, Some(app), _) => Token::App(Box::new(
            AppAuth::new(app, &gh.api, app::system_clock(), app::stderr_log())?
                .with_budget(budget.clone()),
        )),
        (None, None, Some(e)) => Token::Env(e.clone()),
        (None, None, None) => unreachable!("the config checks for a token"),
    })
}

/// A source's HTTP behind the lease fence.
type Gated<T> = lease::Gate<budget::Budgeted<T>>;

async fn github_source(
    gh: &config::GithubConfig,
    lessee: &Arc<lease::Lessee>,
    budget: &Arc<budget::Budget>,
) -> Result<Github<Gated<Reqwest>>> {
    let sources = [
        ("token_file", gh.token_file.is_some()),
        ("app", gh.app.is_some()),
        ("token_env", gh.token_env.is_some()),
    ];
    let set: Vec<&str> = sources.iter().filter(|s| s.1).map(|s| s.0).collect();
    if set.len() > 1 {
        eprintln!(
            "kbl-ingest: warning: several GitHub credentials are configured ({}); {} is used and the rest are ignored",
            set.join(", "),
            set[0]
        );
    }
    let mut github = Github::new(
        &gh.api,
        lease::gated(
            budget::budgeted(Reqwest::new(github_token(gh, budget)?)?, budget.clone()),
            lessee.clone(),
        ),
        gh.own_logins.clone(),
    );
    // A user token names itself; an App installation token cannot, so the config lists its bot login.
    if let Some(login) = lease::identity(github.login()).await {
        github.add_own(login);
    }
    Ok(github)
}

pub(crate) fn jira_auth(j: &config::JiraConfig) -> jira::transport::Auth {
    use jira::transport::{Auth, Secret};
    let secret = match (&j.token_file, &j.token_env) {
        (Some(f), _) => Some(Secret::File(f.clone())),
        (None, Some(e)) => Some(Secret::Env(e.clone())),
        (None, None) => None,
    };
    match (&j.email, secret, &j.oauth_token_file) {
        (Some(email), Some(secret), None) => Auth::Token {
            email: email.clone(),
            secret,
        },
        (None, None, Some(f)) => Auth::Bearer(Secret::File(f.clone())),
        _ => unreachable!("the config checks the sign-in"),
    }
}

pub(crate) fn jira_scopes(cfg: &Config) -> Vec<jira::Scope> {
    cfg.rules
        .iter()
        .filter_map(|r| {
            Some(jira::Scope {
                name: r.name.clone(),
                repo: r.repo.clone(),
                jql: r.jql.clone()?,
            })
        })
        .collect()
}

async fn jira_source(
    j: &config::JiraConfig,
    cfg: &Config,
    lessee: &Arc<lease::Lessee>,
    budget: &Arc<budget::Budget>,
) -> Result<jira::Jira<Gated<jira::transport::Reqwest>>> {
    let mut jira = jira::Jira::new(
        &j.site,
        &j.api,
        lease::gated(
            budget::budgeted(jira::transport::Reqwest::new(jira_auth(j))?, budget.clone()),
            lessee.clone(),
        ),
        j.own_logins.clone(),
        jira_scopes(cfg),
        &j.pr_url_base,
    );
    jira.add_own(lease::identity(jira.login()).await);
    jira.require_own()?;
    if let Some(b) = &j.browse_url {
        jira.browse_at(b);
    }
    Ok(jira)
}

pub(crate) fn ado_auth(
    a: &config::AdoConfig,
    budget: &Arc<budget::Budget>,
) -> ado::transport::Auth {
    use ado::transport::{Auth, Entra, Secret};
    match (&a.pat_file, &a.pat_env, &a.entra) {
        (Some(f), _, None) => Auth::Pat(Secret::File(f.clone())),
        (None, Some(e), None) => Auth::Pat(Secret::Env(e.clone())),
        (None, None, Some(e)) => {
            let secret = match (&e.client_secret_file, &e.client_secret_env) {
                (Some(f), _) => Secret::File(f.clone()),
                (None, Some(v)) => Secret::Env(v.clone()),
                (None, None) => unreachable!("the config checks the client secret"),
            };
            Auth::Entra(
                Entra::new(&e.tenant_id, &e.client_id, secret, &e.authority)
                    .with_budget(budget.clone()),
            )
        }
        _ => unreachable!("the config checks the sign-in"),
    }
}

pub(crate) fn ado_scopes(cfg: &Config) -> Vec<ado::Scope> {
    cfg.rules
        .iter()
        .map(|r| ado::Scope { rule: r.clone() })
        .collect()
}

async fn ado_source(
    a: &config::AdoConfig,
    cfg: &Config,
    lessee: &Arc<lease::Lessee>,
    budget: &Arc<budget::Budget>,
) -> Result<ado::Ado<Gated<ado::transport::Reqwest>>> {
    let plain = std::iter::once(a.api.as_str())
        .chain(a.entra.iter().map(|e| e.authority.as_str()))
        .any(|u| !u.starts_with("https://"));
    if plain {
        eprintln!(
            "kbl-ingest: warning: an Azure DevOps address is not https; the credential is sent in the clear"
        );
    }
    let mut ado = ado::Ado::new(
        &a.organization,
        &a.api,
        lease::gated(
            budget::budgeted(
                ado::transport::Reqwest::new(ado_auth(a, budget))?,
                budget.clone(),
            ),
            lessee.clone(),
        ),
        a.own_logins.clone(),
        ado_scopes(cfg),
        &a.projects,
        &a.pr_url_base,
    );
    ado.add_own(lease::identity(ado.login()).await);
    ado.require_own()?;
    Ok(ado)
}

pub(crate) fn gitlab_secret(g: &config::GitlabConfig) -> ado::transport::Secret {
    use ado::transport::Secret;
    match (&g.token_file, &g.token_env) {
        (Some(f), _) => Secret::File(f.clone()),
        (None, Some(e)) => Secret::Env(e.clone()),
        (None, None) => unreachable!("the config checks for a token"),
    }
}

async fn gitlab_source(
    g: &config::GitlabConfig,
    cfg: &Config,
    lessee: &Arc<lease::Lessee>,
    budget: &Arc<budget::Budget>,
) -> Result<gitlab::Gitlab<Gated<gitlab::transport::Reqwest>>> {
    let mut gitlab = gitlab::Gitlab::new(
        &g.api,
        lease::gated(
            budget::budgeted(
                gitlab::transport::Reqwest::new(gitlab_secret(g))?,
                budget.clone(),
            ),
            lessee.clone(),
        ),
        g.own_logins.clone(),
        cfg.rules.clone(),
        &g.projects,
    )?;
    gitlab.add_own(lease::identity(gitlab.login()).await);
    gitlab.require_own()?;
    Ok(gitlab)
}

async fn drive<S: Source>(
    source: S,
    (tracker, lease_api, lessee): (
        tracker_api::Http,
        Arc<tracker_api::Http>,
        Arc<lease::Lessee>,
    ),
    budget: Arc<budget::Budget>,
    cfg: Config,
    cli: Cli,
) -> Result<()> {
    let repos = cfg.repos();
    let poll_min = cfg.poll_floor();
    let reconcile_period = cfg.reconcile_period();
    let leasing = match cfg.tracker.group.clone() {
        Some(group) => {
            let instance = lease::instance()?;
            tokio::spawn(lease::config_task(
                lease_api.clone(),
                lessee.clone(),
                group.clone(),
                instance.clone(),
            ));
            let beating = tokio::spawn(lease::heartbeat_task(
                lease_api.clone(),
                lessee.clone(),
                group.clone(),
                instance.clone(),
            ));
            tokio::spawn(backoff::task(lease_api.clone(), budget.clone()));
            Some((group, instance, beating))
        }
        None => None,
    };
    let ingest = Ingest::new(
        lease::Fenced::new(source, lessee.clone()),
        tracker,
        Settings {
            rules: cfg.rules,
            repos,
            reconcile_period: Some(reconcile_period),
            release_compares: cfg.release_compares_per_poll,
        },
    );

    let wake = Arc::new(webhook::Wake::default());
    if let Some(addr) = cli.webhook_listen {
        check_listen(addr, cli.webhook_behind_tls_proxy)?;
        let var = cli
            .webhook_secret_env
            .context("a webhook secret variable")?;
        let secret = std::env::var(&var).with_context(|| format!("{var} is not set"))?;
        anyhow::ensure!(!secret.is_empty(), "{var} is empty");
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("listen on {addr}"))?;
        eprintln!("kbl-ingest: webhook on {}", listener.local_addr()?);
        let app = webhook::router(
            secret,
            wake.clone(),
            Duration::from_secs(cli.webhook_min_gap_seconds),
        );
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                eprintln!("kbl-ingest: the webhook server stopped: {e}");
            }
        });
    }

    let done = run(&ingest, &lessee, &wake, &budget, poll_min, cli.once).await;
    // Released only once the poll loop has stopped, so the next holder never
    // overlaps this one.
    if let Some((group, instance, beating)) = leasing {
        lease::leave(&*lease_api, beating, &group, &instance).await;
    }
    done
}

async fn run<S: Source, T: tracker_api::Tracker>(
    ingest: &Ingest<S, T>,
    lessee: &lease::Lessee,
    wake: &webhook::Wake,
    budget: &budget::Budget,
    min_every: Duration,
    once: bool,
) -> Result<()> {
    let mut every = min_every;
    ingest.set_interval(every);
    // The last sleep ended on a delivery, not the interval.
    let mut woken = false;
    loop {
        // Without a fresh lease the poll would make no vendor call anyway;
        // skipping it also spares the tracker.
        if !lessee.may_poll() {
            let changed = lessee.changed.notified();
            tokio::select! {
                () = changed => {}
                () = tokio::time::sleep(every) => {}
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
            continue;
        }
        // The vendor's wait, or a bucket too low to afford a poll, holds the
        // poll back even when a webhook woke it.
        if woken {
            budget.begin_wake_poll();
        } else {
            budget.begin_poll();
        }
        woken = false;
        if !once && let Some(hold) = budget.poll_wait() {
            eprintln!(
                "kbl-ingest: request budget low; next poll in {}s",
                hold.as_secs()
            );
            tokio::select! {
                () = tokio::time::sleep(hold) => {}
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
            continue;
        }
        let reconcile = wake.reconcile.swap(false, Ordering::SeqCst);
        let polled = ingest.poll(reconcile).await;
        budget.end_poll(polled.is_ok());
        // A failed poll did not reconcile, so the next one still must.
        if reconcile && polled.is_err() {
            wake.reconcile.store(true, Ordering::SeqCst);
        }
        every = budget.interval(min_every);
        ingest.set_interval(every);
        let snap = budget.snapshot();
        tracing::info!(
            interval_s = every.as_secs(),
            budget_remaining = snap.tokens,
            budget_per_hour = snap.hourly,
            "poll finished"
        );
        eprintln!(
            "kbl-ingest: next poll in {}s; request budget {} of {} an hour left",
            every.as_secs(),
            snap.tokens,
            snap.hourly
        );
        let wait = match polled {
            Ok(()) => every,
            Err(SourceError::RateLimited { wait }) => {
                eprintln!("kbl-ingest: rate limited; waiting {}s", wait.as_secs());
                wait.max(every)
            }
            Err(e) => {
                if once {
                    anyhow::bail!("{e}");
                }
                eprintln!("kbl-ingest: poll failed: {e}");
                every
            }
        };
        if once {
            return Ok(());
        }
        // A delivery cuts the wait short only while the wake share can pay
        // for the poll; otherwise it waits out the interval like any other.
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => break,
                () = wake.notify.notified() => {
                    if budget.wake_affordable() {
                        woken = true;
                        break;
                    }
                }
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests;
