//! The optional webhook endpoint. A webhook never carries data into the
//! tracker: it only wakes the poll early, so a missed or redelivered one costs
//! latency at most. The signature is checked so a stranger cannot make the
//! worker spend GitHub requests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;

/// What the poll loop waits on.
#[derive(Default)]
pub(crate) struct Wake {
    pub(crate) notify: Notify,
    /// A transfer or deletion was reported, which no listing shows.
    pub(crate) reconcile: AtomicBool,
}

struct App {
    secret: Vec<u8>,
    wake: Arc<Wake>,
}

pub(crate) fn router(secret: String, wake: Arc<Wake>) -> Router {
    Router::new()
        .route("/webhook", post(hook))
        .with_state(Arc::new(App {
            secret: secret.into_bytes(),
            wake,
        }))
}

async fn hook(State(app): State<Arc<App>>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let signed = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("sha256="));
    let Some(signed) = signed else {
        return StatusCode::UNAUTHORIZED;
    };
    if !same(&hex(&hmac_sha256(&app.secret, &body)), signed) {
        return StatusCode::UNAUTHORIZED;
    }
    let action = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v["action"].as_str().map(str::to_owned));
    if matches!(action.as_deref(), Some("transferred" | "deleted")) {
        app.wake.reconcile.store(true, Ordering::SeqCst);
    }
    app.wake.notify.notify_one();
    StatusCode::ACCEPTED
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |x: u8| k.map(|b| b ^ x);
    let mut inner = Sha256::new();
    inner.update(pad(0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(pad(0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Equal in time that depends on the lengths only.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

#[cfg(test)]
mod tests;
