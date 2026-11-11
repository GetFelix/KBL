//! The work tools: how an agent claims, extends, finishes and hands back work
//! items. Every call goes to the tracker over REST with the agent's own token;
//! the tracker does the Felix work and writes the records. See
//! docs/work-tracker.md.
//!
//! While an item is held, this process extends the claim on a timer, the same
//! loop the task tools use.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::felix::ExtendError;
use crate::keepalive::{self, Claim, Health, at_risk};

/// How long a claim stands without an extension, when the tracker does not say.
const VISIBILITY: Duration = Duration::from_secs(90);
/// How often a held claim is extended.
const EXTEND_EVERY: Duration = Duration::from_secs(30);
/// How long a call to the tracker may take. `work_next` waits up to 30 s inside it.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a call to the tracker did not succeed.
#[derive(Debug)]
pub(crate) enum CallError {
    /// The claim lapsed or went to another agent.
    Lost(String),
    /// The tracker refused: the agent does not hold the item, the fold said no.
    Refused(String),
    /// The tracker could not be reached or failed.
    Down(String),
}

/// The tracker's REST API, so the tools' rules can be tested without one.
pub(crate) trait TrackerApi: Clone + Send + Sync + 'static {
    fn call(
        &self,
        path: &str,
        body: Value,
    ) -> impl Future<Output = Result<Value, CallError>> + Send;
}

#[derive(Clone)]
pub(crate) struct Http {
    client: reqwest::Client,
    base: String,
    token: String,
}

impl Http {
    pub(crate) fn new(base: &str, token: String) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: reqwest::Client::builder().timeout(CALL_TIMEOUT).build()?,
            base: base.trim_end_matches('/').to_owned(),
            token,
        })
    }
}

impl TrackerApi for Http {
    async fn call(&self, path: &str, body: Value) -> Result<Value, CallError> {
        let response = self
            .client
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .map_err(|e| CallError::Down(format!("could not reach the tracker: {e}")))?;
        let status = response.status();
        let reply: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(reply);
        }
        let message = reply["error"]
            .as_str()
            .map_or_else(|| format!("the tracker answered {status}"), str::to_owned);
        Err(match (status.as_u16(), reply["why"].as_str()) {
            (409, Some("lost")) => CallError::Lost(message),
            (400..=499, _) => CallError::Refused(message),
            _ => CallError::Down(message),
        })
    }
}

/// The one item this agent holds, and the loop that keeps its claim alive.
struct Held {
    item: String,
    extender: JoinHandle<()>,
    health: Arc<StdMutex<Health>>,
}

struct Inner<T> {
    api: T,
    /// Also taken for the whole of a claim, so two calls cannot both take an item.
    held: Mutex<Option<Held>>,
    visibility: Duration,
    extend_every: Duration,
}

pub(crate) struct Work<T>(Arc<Inner<T>>);

impl<T> Clone for Work<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: TrackerApi> Work<T> {
    pub(crate) fn new(
        api: T,
        visibility: Option<Duration>,
        extend_every: Option<Duration>,
    ) -> Self {
        Self(Arc::new(Inner {
            api,
            held: Mutex::new(None),
            visibility: visibility.unwrap_or(VISIBILITY),
            extend_every: extend_every.unwrap_or(EXTEND_EVERY),
        }))
    }

    pub(crate) async fn next(&self, wait: Option<Duration>) -> Result<String, String> {
        let mut held = self.0.held.lock().await;
        if let Some(lost) = lost(&mut held) {
            return Err(lost);
        }
        if let Some(h) = held.as_ref() {
            let mut msg = format!("you already hold {}; complete or release it first", h.item);
            if let Some(risk) = at_risk(&h.health) {
                msg.push_str(&format!(". {risk}"));
            }
            return Err(msg);
        }
        let body = match wait {
            Some(w) => json!({ "wait_seconds": w.as_secs() }),
            None => json!({}),
        };
        let reply = self
            .0
            .api
            .call("/v1/work/next", body)
            .await
            .map_err(message)?;
        if let Some(item) = reply["claimed"]
            .as_bool()
            .filter(|c| *c)
            .and_then(|_| reply["item"]["id"].as_str())
        {
            let health = Health::fresh();
            let extender = tokio::spawn(keepalive::keep_alive(
                WorkClaim {
                    work: self.clone(),
                    item: item.to_owned(),
                },
                self.0.extend_every,
                self.0.visibility,
                health.clone(),
            ));
            *held = Some(Held {
                item: item.to_owned(),
                extender,
                health,
            });
        }
        Ok(reply.to_string())
    }

    /// The item this agent holds, which the tools that act on "the item" default to.
    pub(crate) async fn held_item(&self) -> Option<String> {
        self.0.held.lock().await.as_ref().map(|h| h.item.clone())
    }

    /// A tool that writes or reads a record through the tracker. A refusal
    /// comes back as the reason and leaves the claim as it was.
    pub(crate) async fn tool(&self, path: &str, body: Value) -> Result<Value, String> {
        self.0.api.call(path, body).await.map_err(message)
    }

    pub(crate) async fn extend(&self, item: &str) -> Result<String, String> {
        self.0
            .api
            .call("/v1/work/extend", json!({ "item": item }))
            .await
            .map(|r| r.to_string())
            .map_err(message)
    }

    pub(crate) async fn complete(&self, item: &str, cost_micros: u64) -> Result<String, String> {
        self.finish(
            "/v1/work/complete",
            json!({ "item": item, "cost_micros": cost_micros }),
            item,
        )
        .await
    }

    pub(crate) async fn release(
        &self,
        item: &str,
        reason: Option<String>,
        cost_micros: u64,
    ) -> Result<String, String> {
        self.finish(
            "/v1/work/release",
            json!({ "item": item, "reason": reason, "cost_micros": cost_micros }),
            item,
        )
        .await
    }

    /// A post that ends the claim, such as a prerequisite.
    pub(crate) async fn finish_post(&self, item: &str, body: Value) -> Result<String, String> {
        self.finish("/v1/work/post", body, item).await
    }

    /// Calls the tracker, then stops extending the claim if the item is settled
    /// or gone. A refusal or an outage leaves the claim held, so the agent can try again.
    async fn finish(&self, path: &str, body: Value, item: &str) -> Result<String, String> {
        let mut held = self.0.held.lock().await;
        let risk = held
            .as_ref()
            .filter(|h| h.item == item)
            .and_then(|h| at_risk(&h.health));
        let result = self.0.api.call(path, body).await;
        let settled = matches!(result, Ok(_) | Err(CallError::Lost(_)));
        if settled && let Some(h) = held.take_if(|h| h.item == item) {
            h.extender.abort();
        }
        let mut reply = result.map_err(message)?;
        if let Some(risk) = risk {
            reply["warning"] = risk.into();
        }
        Ok(reply.to_string())
    }
}

/// A held item's claim, extended through the tracker.
struct WorkClaim<T> {
    work: Work<T>,
    item: String,
}

impl<T: TrackerApi> Claim for WorkClaim<T> {
    async fn extend(&self) -> Result<Duration, ExtendError> {
        let inner = &self.work.0;
        match inner
            .api
            .call("/v1/work/extend", json!({ "item": self.item }))
            .await
        {
            Ok(reply) => Ok(reply["deadline_ms"].as_u64().map_or(inner.visibility, |d| {
                Duration::from_millis(d.saturating_sub(now_ms()))
            })),
            Err(CallError::Lost(m)) => Err(ExtendError::Stale(anyhow::anyhow!(m))),
            Err(CallError::Refused(m) | CallError::Down(m)) => {
                Err(ExtendError::Failed(anyhow::anyhow!(m)))
            }
        }
    }

    async fn extended(&self, _visible: Duration) {}

    async fn failed(&self, error: &anyhow::Error) {
        eprintln!("felix-mcp: extending {} failed: {error:#}", self.item);
    }

    async fn lost(&self) {
        eprintln!("felix-mcp: the claim on {} is lost", self.item);
    }
}

/// If the held item's claim was lost, drops the hold and says so once.
fn lost(held: &mut Option<Held>) -> Option<String> {
    let gone = held
        .as_ref()
        .is_some_and(|h| h.health.lock().expect("health lock").lost);
    if !gone {
        return None;
    }
    let h = held.take()?;
    h.extender.abort();
    Some(format!(
        "your claim on {} lapsed and the tracker may have given it to another agent. Stop working on it; use work_next for new work",
        h.item
    ))
}

fn message(e: CallError) -> String {
    match e {
        CallError::Lost(m) | CallError::Refused(m) | CallError::Down(m) => m,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests;
