//! `agents-ingest`: pulls issues from upstream trackers into the work tracker
//! and keeps them in step. It writes only through the tracker's REST API, under
//! its own token, and makes no model calls. See docs/work-tracker.md "Sources".

mod config;
mod github;
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
use github::transport::{Reqwest, Token};
use source::{Source, SourceError};
use sync::{Ingest, Settings};

#[derive(Parser)]
#[command(version, about = "The upstream issue ingest worker for Felix Agents")]
struct Cli {
    /// JSON file with the tracker, GitHub and intake rules. See docs/work-tracker.md.
    #[arg(long, env = "AGENTS_INGEST_CONFIG")]
    config: PathBuf,
    /// Where the optional webhook endpoint listens. Webhooks only wake the poll.
    #[arg(
        long,
        env = "AGENTS_INGEST_WEBHOOK_LISTEN",
        requires = "webhook_secret_env"
    )]
    webhook_listen: Option<SocketAddr>,
    /// The environment variable holding the webhook secret.
    #[arg(long, env = "AGENTS_INGEST_WEBHOOK_SECRET_ENV")]
    webhook_secret_env: Option<String>,
    /// The least seconds between two webhook wakes of the poll.
    #[arg(
        long,
        env = "AGENTS_INGEST_WEBHOOK_MIN_GAP_SECONDS",
        default_value_t = 10
    )]
    webhook_min_gap_seconds: u64,
    /// Poll once and exit.
    #[arg(long)]
    once: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load(&cli.config)?;
    let token = std::env::var(&cfg.tracker.token_env)
        .with_context(|| format!("{} is not set", cfg.tracker.token_env))?;
    let tracker = tracker_api::Http::new(&cfg.tracker.url, token)?;
    let gh_token = match (&cfg.github.token_file, &cfg.github.token_env) {
        (Some(f), _) => Token::File(f.clone()),
        (None, Some(e)) => Token::Env(e.clone()),
        (None, None) => unreachable!("the config checks for a token"),
    };
    let mut github = Github::new(
        &cfg.github.api,
        Reqwest::new(gh_token)?,
        cfg.github.own_logins.clone(),
    );
    // A user token names itself; an App installation token cannot, so the config lists its bot login.
    if let Some(login) = github.login().await {
        github.add_own(login);
    }

    let repos = cfg.repos();
    let poll_every = Duration::from_secs(cfg.poll_seconds);
    let ingest = Ingest::new(
        github,
        tracker,
        Settings {
            rules: cfg.rules,
            repos,
            reconcile_every: cfg.reconcile_every,
        },
    );

    let wake = Arc::new(webhook::Wake::default());
    if let Some(addr) = cli.webhook_listen {
        let var = cli
            .webhook_secret_env
            .context("a webhook secret variable")?;
        let secret = std::env::var(&var).with_context(|| format!("{var} is not set"))?;
        anyhow::ensure!(!secret.is_empty(), "{var} is empty");
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("listen on {addr}"))?;
        eprintln!("agents-ingest: webhook on {}", listener.local_addr()?);
        let app = webhook::router(
            secret,
            wake.clone(),
            Duration::from_secs(cli.webhook_min_gap_seconds),
        );
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                eprintln!("agents-ingest: the webhook server stopped: {e}");
            }
        });
    }

    run(&ingest, &wake, poll_every, cli.once).await
}

async fn run<S: Source, T: tracker_api::Tracker>(
    ingest: &Ingest<S, T>,
    wake: &webhook::Wake,
    every: Duration,
    once: bool,
) -> Result<()> {
    loop {
        let reconcile = wake.reconcile.swap(false, Ordering::SeqCst);
        let polled = ingest.poll(reconcile).await;
        // A failed poll did not reconcile, so the next one still must.
        if reconcile && polled.is_err() {
            wake.reconcile.store(true, Ordering::SeqCst);
        }
        let wait = match polled {
            Ok(()) => every,
            Err(SourceError::RateLimited { wait }) => {
                eprintln!("agents-ingest: rate limited; waiting {}s", wait.as_secs());
                wait.max(every)
            }
            Err(e) => {
                if once {
                    anyhow::bail!("{e}");
                }
                eprintln!("agents-ingest: poll failed: {e}");
                every
            }
        };
        if once {
            return Ok(());
        }
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = wake.notify.notified() => {}
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}
