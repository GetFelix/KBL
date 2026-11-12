//! Talking to Felix: sign in, create a run's resources through the control
//! plane, then use the broker for the parts only it does.

use std::net::ToSocketAddrs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use felix_client::{
    AckMode, Client, ClientConfig, StartPosition, quic_client_config, root_store_from_pem_file,
};
use serde_json::{Value, json};

/// Where Felix is and who `agents-tool` is.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub(crate) tenant: String,
    pub(crate) namespace: String,
    pub(crate) control_plane: String,
    pub(crate) idp: String,
    /// An ID token to use instead of asking the development IdP for one.
    pub(crate) id_token: Option<String>,
    pub(crate) broker: String,
    pub(crate) broker_ca: String,
    /// The name the broker's certificate is checked against.
    pub(crate) broker_name: String,
}

/// The audience every ID token from the development IdP is issued for.
const ID_TOKEN_AUDIENCE: &str = "kbl";
/// How long one request to Felix may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a new stream may take to get a broker.
const PLACEMENT_WAIT: Duration = Duration::from_secs(30);
/// The development stack's administrator, who may create runs.
const ADMIN_SUBJECT: &str = "agents-admin";

/// How much of a create request was new.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Every item was created: the run is new.
    AllNew,
    /// Every item was already there: the run exists.
    NoneNew,
    /// Some items existed with the same settings and the rest were created.
    Mixed,
}

/// Reads the control plane's reply to a batch create.
fn outcome(body: &Value) -> Result<Outcome> {
    let mut new = 0;
    let mut old = 0;
    for kind in ["streams", "caches"] {
        let items = body[kind]
            .as_array()
            .with_context(|| format!("the reply has no `{kind}` list"))?;
        for item in items {
            match item["status"].as_str() {
                Some("created") => new += 1,
                Some("unchanged") => old += 1,
                other => bail!("the reply gives an item the status {other:?}"),
            }
        }
    }
    Ok(match (new, old) {
        (_, 0) => Outcome::AllNew,
        (0, _) => Outcome::NoneNew,
        _ => Outcome::Mixed,
    })
}

pub(crate) struct Session {
    target: Target,
    http: reqwest::Client,
    id_token: String,
}

impl Session {
    pub(crate) async fn sign_in(target: Target) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        let id_token = match &target.id_token {
            Some(token) => token.clone(),
            None => {
                let url = format!(
                    "{}/token?sub={ADMIN_SUBJECT}&aud={ID_TOKEN_AUDIENCE}",
                    target.idp
                );
                let body: Value = send(http.get(&url), "ask the identity provider for a token")
                    .await?
                    .json()
                    .await?;
                string(&body, "id_token")?
            }
        };
        Ok(Self {
            target,
            http,
            id_token,
        })
    }

    /// A Felix token for `audience`: `felix-controlplane` or `felix-broker`.
    async fn exchange(&self, audience: &str) -> Result<String> {
        let url = format!(
            "{}/v1/tenants/{}/token/exchange",
            self.target.control_plane, self.target.tenant
        );
        let request = self
            .http
            .post(url)
            .bearer_auth(&self.id_token)
            .json(&json!({ "audience": audience }));
        let body: Value = send(request, "exchange the identity token")
            .await?
            .json()
            .await?;
        string(&body, "felix_token")
    }

    /// Creates the batch and says how much of it was new. The control plane
    /// leaves what already exists alone and creates the rest.
    pub(crate) async fn create_resources(&self, batch: &Value) -> Result<Outcome> {
        let token = self.exchange("felix-controlplane").await?;
        let url = format!(
            "{}/v1/tenants/{}/namespaces/{}/resources",
            self.target.control_plane, self.target.tenant, self.target.namespace
        );
        let request = self.http.post(url).bearer_auth(token).json(batch);
        let body: Value = send(request, "create the run's resources")
            .await?
            .json()
            .await?;
        outcome(&body)
    }

    pub(crate) async fn broker(&self) -> Result<Broker> {
        let token = self.exchange("felix-broker").await?;
        let roots = root_store_from_pem_file(Path::new(&self.target.broker_ca))
            .context("read the broker's certificate")?;
        let mut config =
            ClientConfig::optimized_defaults(quic_client_config(Some(Arc::new(roots)), false)?);
        config.auth_tenant_id = Some(self.target.tenant.clone());
        config.auth_token = Some(token);
        // The ack carries the record's offset only once it is committed.
        config.ack_on_commit = true;
        let seeds: Vec<_> = self
            .target
            .broker
            .to_socket_addrs()
            .with_context(|| format!("resolve {}", self.target.broker))?
            .collect();
        let client = tokio::time::timeout(
            REQUEST_TIMEOUT,
            Client::connect_any(&seeds, &self.target.broker_name, config),
        )
        .await
        .context("the broker did not answer in time")?
        .with_context(|| format!("connect to the broker at {}", self.target.broker))?;
        Ok(Broker {
            client: Arc::new(client),
            tenant: self.target.tenant.clone(),
            namespace: self.target.namespace.clone(),
        })
    }
}

pub(crate) struct Broker {
    client: Arc<Client>,
    tenant: String,
    namespace: String,
}

impl Broker {
    /// Creates `group` on a one-shard stream, reading from the first record, so
    /// a task posted before any worker joins is still handed out.
    pub(crate) async fn create_group(&self, stream: &str, group: &str) -> Result<()> {
        placed(|| {
            self.client.group_create(
                &self.tenant,
                &self.namespace,
                stream,
                0,
                group,
                StartPosition::Earliest,
            )
        })
        .await
        .with_context(|| format!("create group {group} on {stream}"))?;
        Ok(())
    }

    /// Every committed record of a one-shard stream, oldest first.
    pub(crate) async fn read_all(&self, stream: &str) -> Result<Vec<Vec<u8>>> {
        let mut payloads = Vec::new();
        let mut from = 0;
        loop {
            let page = self
                .client
                .read(&self.tenant, &self.namespace, stream, 0, from, None, 0)
                .await
                .with_context(|| format!("read {stream} from offset {from}"))?;
            if page.records.is_empty() {
                return Ok(payloads);
            }
            payloads.extend(page.records.into_iter().map(|r| r.payload.to_vec()));
            anyhow::ensure!(
                page.next_offset > from,
                "reading {stream} did not advance past offset {from}"
            );
            from = page.next_offset;
        }
    }

    /// Publishes `payload` and returns the offset the broker gave it.
    pub(crate) async fn publish(&self, stream: &str, payload: Vec<u8>) -> Result<u64> {
        let publisher = self.client.publisher().await?;
        let offset = placed(|| {
            publisher.publish(
                &self.tenant,
                &self.namespace,
                stream,
                payload.clone(),
                AckMode::PerMessage,
            )
        })
        .await
        .with_context(|| format!("publish to {stream}"))?;
        offset.context("the broker did not report an offset")
    }
}

/// What a broker says while a new stream's shard has no owner yet or is still opening.
const NOT_READY: [&str; 2] = ["not assigned", "still opening"];

/// Runs `op`, trying again while the broker says the shard is not ready. The
/// control plane places a new stream's shard after it answers the create. No
/// other failure is retried: a second publish could land twice.
async fn placed<T, F, Fut>(mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let deadline = Instant::now() + PLACEMENT_WAIT;
    loop {
        match op().await {
            Err(err) if Instant::now() < deadline && is_not_ready(&err) => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            done => return done,
        }
    }
}

fn is_not_ready(err: &anyhow::Error) -> bool {
    let text = format!("{err:#}");
    NOT_READY.iter().any(|marker| text.contains(marker))
}

async fn send(request: reqwest::RequestBuilder, doing: &str) -> Result<reqwest::Response> {
    let response = request
        .send()
        .await
        .with_context(|| format!("could not {doing}"))?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let text = response.text().await.unwrap_or_default();
    bail!("could not {doing}: {status}: {text}")
}

fn string(body: &Value, field: &str) -> Result<String> {
    body[field]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("the reply has no `{field}`"))
}

#[cfg(test)]
mod tests;
