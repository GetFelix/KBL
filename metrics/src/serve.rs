//! The `/metrics` listener. It is its own port, never the service's API
//! listener, so a token-bearing port is not also a scrape target.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;

use crate::Exporter;

/// The routes: `GET /metrics` and nothing else.
pub fn router(exporter: Exporter) -> Router {
    Router::new().route(
        "/metrics",
        get(move || {
            let exporter = exporter.clone();
            async move {
                (
                    StatusCode::OK,
                    [(
                        header::CONTENT_TYPE,
                        "text/plain; version=0.0.4; charset=utf-8",
                    )],
                    exporter.render(),
                )
                    .into_response()
            }
        }),
    )
}

/// The endpoint has no authentication, so it stays on loopback unless the
/// operator says the port is reachable only by the scraper.
pub fn check_listen(addr: SocketAddr, allow_remote: bool, flag: &str) -> Result<()> {
    anyhow::ensure!(
        allow_remote || addr.ip().to_canonical().is_loopback(),
        "the metrics endpoint has no authentication, so it listens only on loopback; {addr} is not. \
         Restrict the port to your scraper and set {flag}"
    );
    Ok(())
}

/// Binds `addr` and serves `/metrics` on a background task.
pub async fn serve(addr: SocketAddr, exporter: Exporter) -> Result<SocketAddr> {
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("listen for metrics on {addr}"))?;
    let bound = listener.local_addr()?;
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router(exporter)).await {
            eprintln!("the metrics server stopped: {e}");
        }
    });
    Ok(bound)
}
