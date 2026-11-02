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
}

/// The audience every ID token from the development IdP is issued for.
const ID_TOKEN_AUDIENCE: &str = "felix-agents";
/// How long a new stream may take to get a broker.
const PLACEMENT_WAIT: Duration = Duration::from_secs(30);
/// The development stack's administrator, who may create runs.
const ADMIN_SUBJECT: &str = "agents-admin";

pub(crate) struct Session {
    target: Target,
    http: reqwest::Client,
    id_token: String,
}

impl Session {
    pub(crate) async fn sign_in(target: Target) -> Result<Self> {
        let http = reqwest::Client::new();
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

    /// Creates the batch and reports whether every item was new. A batch that
    /// finds some of it already there changes nothing, so a repeat is `false`.
    pub(crate) async fn create_resources(&self, batch: &Value) -> Result<bool> {
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
        let all_new = ["streams", "caches"].iter().all(|kind| {
            body[kind]
                .as_array()
                .is_some_and(|items| items.iter().all(|item| item["status"] == "created"))
        });
        Ok(all_new)
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
        let client = Client::connect_any(&seeds, "localhost", config)
            .await
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
