//! The tracker's connection to Felix: sign in, create its streams, and the
//! few broker calls the claim path needs.

use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agents_core::work::{READY_GROUP, TIERS, WORK_EVENTS, ready_stream};
use anyhow::{Context, Result, bail};
use felix_client::{
    AckMode, BrokerError, Client, ClientConfig, ErrorCode, GroupMember, GroupPollOptions,
    GroupRecord, StartPosition, quic_client_config, root_store_from_pem_file,
};
use serde_json::{Value, json};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a new stream may take to get a broker.
const PLACEMENT_WAIT: Duration = Duration::from_secs(30);
const ID_TOKEN_AUDIENCE: &str = "felix-agents";
/// The tenant administrator stands in for the tracker's principal in the dev stack.
const TRACKER_SUBJECT: &str = "agents-admin";

/// Where Felix is and who the tracker signs in as.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub(crate) tenant: String,
    pub(crate) namespace: String,
    pub(crate) control_plane: String,
    pub(crate) idp: String,
    pub(crate) id_token: Option<String>,
    pub(crate) broker: String,
    pub(crate) broker_ca: PathBuf,
    pub(crate) broker_name: String,
}

/// Why an extension failed.
#[derive(Debug)]
pub(crate) enum ExtendError {
    /// The claim lapsed or was handed out again.
    Stale,
    Failed(anyhow::Error),
}

/// What the claim path needs from Felix, so its rules can be tested without a
/// broker. Every stream has one shard.
pub(crate) trait Broker: Clone + Send + Sync + 'static {
    /// Publishes and returns the offset the record landed at.
    fn publish(&self, stream: &str, payload: Vec<u8>) -> impl Future<Output = Result<u64>> + Send;

    /// Every committed record from `from`, oldest first. Offsets can skip.
    fn read_from(
        &self,
        stream: &str,
        from: u64,
    ) -> impl Future<Output = Result<Vec<(u64, Vec<u8>)>>> + Send;

    /// Takes at most one record for `member` without waiting.
    fn poll(
        &self,
        stream: &str,
        member: &str,
        visibility: Duration,
    ) -> impl Future<Output = Result<Option<GroupRecord>>> + Send;

    fn ack(&self, stream: &str, offset: u64) -> impl Future<Output = Result<()>> + Send;

    fn nack(&self, stream: &str, record: &GroupRecord) -> impl Future<Output = Result<()>> + Send;

    fn extend(
        &self,
        stream: &str,
        record: &GroupRecord,
        by: Duration,
    ) -> impl Future<Output = Result<Duration, ExtendError>> + Send;
}

#[derive(Clone)]
pub(crate) struct Felix {
    client: Arc<Client>,
    tenant: String,
    namespace: String,
}

/// Signs in, creates the tracker's streams and consumer groups if they are not
/// there yet, and returns the broker connection.
pub(crate) async fn connect(target: Target) -> Result<Felix> {
    let http = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()?;
    let id_token = match &target.id_token {
        Some(token) => token.clone(),
        None => {
            let url = format!(
                "{}/token?sub={TRACKER_SUBJECT}&aud={ID_TOKEN_AUDIENCE}",
                target.idp
            );
            let body: Value = send(http.get(&url), "ask the identity provider for a token")
                .await?
                .json()
                .await?;
            string(&body, "id_token")?
        }
    };
    let exchange = |audience: &'static str| {
        let request = http
            .post(format!(
                "{}/v1/tenants/{}/token/exchange",
                target.control_plane, target.tenant
            ))
            .bearer_auth(&id_token)
            .json(&json!({ "audience": audience }));
        async move {
            let body: Value = send(request, "exchange the identity token")
                .await?
                .json()
                .await?;
            string(&body, "felix_token")
        }
    };

    let admin = exchange("felix-controlplane").await?;
    let url = format!(
        "{}/v1/tenants/{}/namespaces/{}/resources",
        target.control_plane, target.tenant, target.namespace
    );
    send(
        http.post(url).bearer_auth(admin).json(&streams_batch()),
        "create the work streams",
    )
    .await?;

    let token = exchange("felix-broker").await?;
    let roots = root_store_from_pem_file(&target.broker_ca).with_context(|| {
        format!(
            "read the broker's certificate {}",
            target.broker_ca.display()
        )
    })?;
    let mut config =
        ClientConfig::optimized_defaults(quic_client_config(Some(Arc::new(roots)), false)?);
    config.auth_tenant_id = Some(target.tenant.clone());
    config.auth_token = Some(token);
    // The ack carries the record's offset only once it is committed.
    config.ack_on_commit = true;
    let seeds: Vec<_> = target
        .broker
        .to_socket_addrs()
        .with_context(|| format!("resolve {}", target.broker))?
        .collect();
    let client = tokio::time::timeout(
        REQUEST_TIMEOUT,
        Client::connect_any(&seeds, &target.broker_name, config),
    )
    .await
    .context("the broker did not answer in time")?
    .with_context(|| format!("connect to the broker at {}", target.broker))?;
    let felix = Felix {
        client: Arc::new(client),
        tenant: target.tenant,
        namespace: target.namespace,
    };
    for tier in 0..TIERS {
        felix.ensure_group(&ready_stream(tier)).await?;
    }
    Ok(felix)
}

/// The log and the four ready tiers: one durable stream each.
fn streams_batch() -> Value {
    let streams: Vec<Value> = std::iter::once(WORK_EVENTS.to_owned())
        .chain((0..TIERS).map(ready_stream))
        .map(|name| {
            json!({
                "stream": name,
                "kind": "Stream",
                "shards": 1,
                "replication_factor": 1,
                "retention": { "max_age_seconds": null, "max_size_bytes": null },
                "consistency": "Leader",
                "delivery": "AtLeastOnce",
                "durable": true,
            })
        })
        .collect();
    json!({ "streams": streams, "caches": [] })
}

impl Felix {
    /// Creates the group from the first record, unless it already exists.
    async fn ensure_group(&self, stream: &str) -> Result<()> {
        let created = placed(|| {
            self.client.group_create(
                &self.tenant,
                &self.namespace,
                stream,
                0,
                READY_GROUP,
                StartPosition::Earliest,
            )
        })
        .await;
        if created.is_err() {
            // A restart finds the group there; anything else shows up as a failed describe.
            placed(|| {
                self.client
                    .group_describe(&self.tenant, &self.namespace, stream, 0, READY_GROUP)
            })
            .await
            .with_context(|| format!("create group {READY_GROUP} on {stream}"))?;
        }
        Ok(())
    }
}

impl Broker for Felix {
    async fn publish(&self, stream: &str, payload: Vec<u8>) -> Result<u64> {
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

    async fn read_from(&self, stream: &str, from: u64) -> Result<Vec<(u64, Vec<u8>)>> {
        let mut out = Vec::new();
        let mut from = from;
        loop {
            let page = placed(|| {
                self.client
                    .read(&self.tenant, &self.namespace, stream, 0, from, None, 0)
            })
            .await
            .with_context(|| format!("read {stream} from offset {from}"))?;
            if page.records.is_empty() {
                return Ok(out);
            }
            out.extend(
                page.records
                    .into_iter()
                    .map(|r| (r.offset, r.payload.to_vec())),
            );
            anyhow::ensure!(
                page.next_offset > from,
                "reading {stream} did not advance past offset {from}"
            );
            from = page.next_offset;
        }
    }

    async fn poll(
        &self,
        stream: &str,
        member: &str,
        visibility: Duration,
    ) -> Result<Option<GroupRecord>> {
        let options = GroupPollOptions {
            member: Some(GroupMember {
                consumer: member.to_owned(),
                // The broker acts on this in a connection's first poll only.
                reclaim: true,
            }),
            wait: Duration::ZERO,
            visibility: Some(visibility),
        };
        let records = self
            .client
            .group_poll_with(
                &self.tenant,
                &self.namespace,
                stream,
                0,
                READY_GROUP,
                1,
                &options,
            )
            .await?;
        Ok(records.into_iter().next())
    }

    async fn ack(&self, stream: &str, offset: u64) -> Result<()> {
        self.client
            .group_ack(
                &self.tenant,
                &self.namespace,
                stream,
                0,
                READY_GROUP,
                offset,
            )
            .await
    }

    async fn nack(&self, stream: &str, record: &GroupRecord) -> Result<()> {
        self.client
            .group_nack(
                &self.tenant,
                &self.namespace,
                stream,
                0,
                READY_GROUP,
                record,
            )
            .await
    }

    async fn extend(
        &self,
        stream: &str,
        record: &GroupRecord,
        by: Duration,
    ) -> Result<Duration, ExtendError> {
        self.client
            .group_extend(
                &self.tenant,
                &self.namespace,
                stream,
                0,
                READY_GROUP,
                record,
                by,
            )
            .await
            .map_err(|e| {
                // Felix's typed refusal, not its wording, says the claim is gone.
                let stale = e
                    .downcast_ref::<BrokerError>()
                    .is_some_and(|b| b.code == ErrorCode::StaleClaim);
                if stale {
                    ExtendError::Stale
                } else {
                    ExtendError::Failed(e)
                }
            })
    }
}

/// What a broker says while a new stream's shard has no owner yet or is still opening.
const NOT_READY: [&str; 2] = ["not assigned", "still opening"];

/// Runs `op`, trying again while the broker says the shard is not ready. No
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
