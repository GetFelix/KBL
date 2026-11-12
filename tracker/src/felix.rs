//! The tracker's connection to Felix: sign in, create its streams, and the
//! few broker calls the claim path needs.

use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use felix_client::{
    AckMode, BrokerError, CacheCondition, Client, ClientConfig, ErrorCode, GroupMember,
    GroupPollOptions, GroupRecord, StartPosition, quic_client_config, root_store_from_pem_file,
};
use kbl_core::work::{LANES, READY_GROUP, WORK_EVENTS, ready_stream};
use serde_json::{Value, json};

/// The cache and key that hold the tracker's writer lease.
const LEASE_CACHE: &str = "work.tracker";
const LEASE_KEY: &str = "writer";

/// The key ingest's state for source `name` is kept under, in the lease cache.
fn cursor_key(name: &str) -> String {
    format!("ingest-{name}")
}
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a new stream may take to get a broker.
const PLACEMENT_WAIT: Duration = Duration::from_secs(30);
const ID_TOKEN_AUDIENCE: &str = "kbl";
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

    /// Takes at most one record for `member` without waiting. The first poll
    /// of a member reclaims what Felix still holds for that name, so each
    /// member has a connection of its own.
    fn poll(
        &self,
        stream: &str,
        member: &str,
        visibility: Duration,
    ) -> impl Future<Output = Result<Option<GroupRecord>>> + Send;

    /// `member` is the agent the delivery was made to.
    fn ack(
        &self,
        stream: &str,
        member: &str,
        offset: u64,
    ) -> impl Future<Output = Result<()>> + Send;

    fn nack(
        &self,
        stream: &str,
        member: &str,
        record: &GroupRecord,
    ) -> impl Future<Output = Result<()>> + Send;

    fn extend(
        &self,
        stream: &str,
        member: &str,
        record: &GroupRecord,
        by: Duration,
    ) -> impl Future<Output = Result<Duration, ExtendError>> + Send;

    /// Writes the writer lease key with a time to live, only if it has no live
    /// entry (`expect` none) or is at `expect`'s version. Returns the new
    /// version when the write was made.
    fn lease_put(
        &self,
        holder: Vec<u8>,
        ttl: Duration,
        expect: Option<u64>,
    ) -> impl Future<Output = Result<Option<u64>>> + Send;

    /// The value ingest keeps under `name` and its version.
    fn cursor_get(
        &self,
        _name: &str,
    ) -> impl Future<Output = Result<Option<(Vec<u8>, u64)>>> + Send {
        async { bail!("this broker keeps no ingest cursors") }
    }

    /// Writes ingest's value under `name` if its version is still `expect`
    /// (none: the key must be absent). Returns the new version, or none when
    /// the condition failed.
    fn cursor_put(
        &self,
        _name: &str,
        _value: Vec<u8>,
        _expect: Option<u64>,
    ) -> impl Future<Output = Result<Option<u64>>> + Send {
        async { bail!("this broker keeps no ingest cursors") }
    }

    /// Drops a connection that failed or timed out, so the next call opens a
    /// new one: `member`'s, or the log's when none.
    fn forget(&self, member: Option<&str>) -> impl Future<Output = ()> + Send;

    /// Closes the connections of agents that have made no call for `idle`.
    fn close_idle(&self, idle: Duration) -> impl Future<Output = ()> + Send;
}

/// A connection to the broker and when it was last used.
struct Conn {
    client: Arc<Client>,
    last_used: Instant,
}

/// Signs in again whenever a connection is opened, so a restart of a
/// connection after the broker token expired still works.
struct Session {
    target: Target,
    http: reqwest::Client,
    seeds: Vec<std::net::SocketAddr>,
}

impl Session {
    async fn exchange(&self, audience: &'static str) -> Result<String> {
        let id_token = match &self.target.id_token {
            Some(token) => token.clone(),
            None => {
                let url = format!(
                    "{}/token?sub={TRACKER_SUBJECT}&aud={ID_TOKEN_AUDIENCE}",
                    self.target.idp
                );
                let body: Value =
                    send(self.http.get(&url), "ask the identity provider for a token")
                        .await?
                        .json()
                        .await?;
                string(&body, "id_token")?
            }
        };
        let request = self
            .http
            .post(format!(
                "{}/v1/tenants/{}/token/exchange",
                self.target.control_plane, self.target.tenant
            ))
            .bearer_auth(&id_token)
            .json(&json!({ "audience": audience }));
        let body: Value = send(request, "exchange the identity token")
            .await?
            .json()
            .await?;
        string(&body, "felix_token")
    }

    /// A new connection with a freshly exchanged token.
    async fn open(&self) -> Result<Arc<Client>> {
        let token = self.exchange("felix-broker").await?;
        let roots = root_store_from_pem_file(&self.target.broker_ca).with_context(|| {
            format!(
                "read the broker's certificate {}",
                self.target.broker_ca.display()
            )
        })?;
        let mut config =
            ClientConfig::optimized_defaults(quic_client_config(Some(Arc::new(roots)), false)?);
        config.auth_tenant_id = Some(self.target.tenant.clone());
        config.auth_token = Some(token);
        // The ack carries the record's offset only once it is committed.
        config.ack_on_commit = true;
        let client = tokio::time::timeout(
            REQUEST_TIMEOUT,
            Client::connect_any(&self.seeds, &self.target.broker_name, config),
        )
        .await
        .context("the broker did not answer in time")?
        .with_context(|| format!("connect to the broker at {}", self.target.broker))?;
        Ok(Arc::new(client))
    }
}

/// Each agent that polls has a broker connection of its own, because the broker
/// reclaims a member's deliveries only in a connection's first poll. That is
/// one QUIC connection per active agent, opened on its first call and closed
/// after ten minutes without one. A connection that fails is dropped and
/// opened again, with a fresh token, on the next call.
#[derive(Clone)]
pub(crate) struct Felix {
    session: Arc<Session>,
    /// The connection for publishing and reading the log.
    main: Arc<tokio::sync::Mutex<Option<Conn>>>,
    members: Arc<tokio::sync::Mutex<HashMap<String, Conn>>>,
}

/// Signs in, creates the tracker's streams, cache and consumer groups if they
/// are not there yet, and returns the broker connection.
pub(crate) async fn connect(target: Target) -> Result<Felix> {
    let http = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()?;
    let seeds: Vec<_> = target
        .broker
        .to_socket_addrs()
        .with_context(|| format!("resolve {}", target.broker))?
        .collect();
    let session = Arc::new(Session {
        target,
        http,
        seeds,
    });

    let admin = session.exchange("felix-controlplane").await?;
    let url = format!(
        "{}/v1/tenants/{}/namespaces/{}/resources",
        session.target.control_plane, session.target.tenant, session.target.namespace
    );
    send(
        session
            .http
            .post(url)
            .bearer_auth(admin)
            .json(&streams_batch()),
        "create the work streams",
    )
    .await?;

    let felix = Felix {
        session,
        main: Arc::default(),
        members: Arc::default(),
    };
    for lane in 0..LANES {
        felix.ensure_group(&ready_stream(lane)).await?;
    }
    Ok(felix)
}

/// The log and the ready lanes: one durable stream each, and the lease cache.
fn streams_batch() -> Value {
    let streams: Vec<Value> = std::iter::once(WORK_EVENTS.to_owned())
        .chain((0..LANES).map(ready_stream))
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
    let lease = json!({
        "cache": LEASE_CACHE,
        "display_name": LEASE_CACHE,
        "shards": 1,
        "replication_factor": 1,
        "consistency": "Leader",
    });
    json!({ "streams": streams, "caches": [lease] })
}

impl Felix {
    fn tenant(&self) -> &str {
        &self.session.target.tenant
    }

    fn namespace(&self) -> &str {
        &self.session.target.namespace
    }

    /// The log connection, opened if there is none.
    async fn main(&self) -> Result<Arc<Client>> {
        let mut main = self.main.lock().await;
        if let Some(c) = main.as_mut() {
            c.last_used = Instant::now();
            return Ok(c.client.clone());
        }
        let client = self.session.open().await?;
        *main = Some(Conn {
            client: client.clone(),
            last_used: Instant::now(),
        });
        Ok(client)
    }

    /// The connection for `member`, opened on first use.
    async fn member(&self, member: &str) -> Result<Arc<Client>> {
        let mut members = self.members.lock().await;
        if let Some(c) = members.get_mut(member) {
            c.last_used = Instant::now();
            return Ok(c.client.clone());
        }
        let client = self
            .session
            .open()
            .await
            .with_context(|| format!("open a connection for {member}"))?;
        members.insert(
            member.to_owned(),
            Conn {
                client: client.clone(),
                last_used: Instant::now(),
            },
        );
        Ok(client)
    }

    /// Drops a connection that failed, so the next call opens a new one.
    async fn forget_main<T>(&self, result: Result<T>) -> Result<T> {
        if result.is_err() {
            *self.main.lock().await = None;
        }
        result
    }

    async fn forget_member<T>(&self, member: &str, result: Result<T>) -> Result<T> {
        if result.is_err() {
            self.members.lock().await.remove(member);
        }
        result
    }

    /// Creates the group from the first record, unless it already exists.
    async fn ensure_group(&self, stream: &str) -> Result<()> {
        let client = self.main().await?;
        let (tenant, namespace) = (self.tenant(), self.namespace());
        let created = placed(|| {
            client.group_create(
                tenant,
                namespace,
                stream,
                0,
                READY_GROUP,
                StartPosition::Earliest,
            )
        })
        .await;
        if created.is_err() {
            // A restart finds the group there; anything else shows up as a failed describe.
            placed(|| client.group_describe(tenant, namespace, stream, 0, READY_GROUP))
                .await
                .with_context(|| format!("create group {READY_GROUP} on {stream}"))?;
        }
        Ok(())
    }
}

impl Broker for Felix {
    async fn publish(&self, stream: &str, payload: Vec<u8>) -> Result<u64> {
        let done = async {
            let client = self.main().await?;
            let publisher = client.publisher().await?;
            let offset = placed(|| {
                publisher.publish(
                    self.tenant(),
                    self.namespace(),
                    stream,
                    payload.clone(),
                    AckMode::PerMessage,
                )
            })
            .await
            .with_context(|| format!("publish to {stream}"))?;
            offset.context("the broker did not report an offset")
        }
        .await;
        self.forget_main(done).await
    }

    async fn read_from(&self, stream: &str, from: u64) -> Result<Vec<(u64, Vec<u8>)>> {
        let done = async {
            let client = self.main().await?;
            let mut out = Vec::new();
            let mut from = from;
            loop {
                let page = placed(|| {
                    client.read(self.tenant(), self.namespace(), stream, 0, from, None, 0)
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
        .await;
        self.forget_main(done).await
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
        let done = async {
            let client = self.member(member).await?;
            let records = client
                .group_poll_with(
                    self.tenant(),
                    self.namespace(),
                    stream,
                    0,
                    READY_GROUP,
                    1,
                    &options,
                )
                .await?;
            Ok(records.into_iter().next())
        }
        .await;
        self.forget_member(member, done).await
    }

    async fn ack(&self, stream: &str, member: &str, offset: u64) -> Result<()> {
        let done = async {
            self.member(member)
                .await?
                .group_ack(
                    self.tenant(),
                    self.namespace(),
                    stream,
                    0,
                    READY_GROUP,
                    offset,
                )
                .await
        }
        .await;
        self.forget_member(member, done).await
    }

    async fn nack(&self, stream: &str, member: &str, record: &GroupRecord) -> Result<()> {
        let done = async {
            self.member(member)
                .await?
                .group_nack(
                    self.tenant(),
                    self.namespace(),
                    stream,
                    0,
                    READY_GROUP,
                    record,
                )
                .await
        }
        .await;
        self.forget_member(member, done).await
    }

    async fn extend(
        &self,
        stream: &str,
        member: &str,
        record: &GroupRecord,
        by: Duration,
    ) -> Result<Duration, ExtendError> {
        let client = self.member(member).await.map_err(ExtendError::Failed)?;
        client
            .group_extend(
                self.tenant(),
                self.namespace(),
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

    async fn lease_put(
        &self,
        holder: Vec<u8>,
        ttl: Duration,
        expect: Option<u64>,
    ) -> Result<Option<u64>> {
        let condition = match expect {
            None => CacheCondition::Absent,
            Some(v) => CacheCondition::Version(v),
        };
        let done = async {
            let put = self
                .main()
                .await?
                .cache_put_if(
                    self.tenant(),
                    self.namespace(),
                    LEASE_CACHE,
                    LEASE_KEY,
                    Bytes::from(holder),
                    Some(ttl.as_millis() as u64),
                    condition,
                )
                .await?;
            Ok(put.applied.then_some(put.version).flatten())
        }
        .await;
        self.forget_main(done).await
    }

    async fn cursor_get(&self, name: &str) -> Result<Option<(Vec<u8>, u64)>> {
        let done = async {
            let got = self
                .main()
                .await?
                .cache_get_versioned(
                    self.tenant(),
                    self.namespace(),
                    LEASE_CACHE,
                    &cursor_key(name),
                )
                .await?;
            Ok(got.map(|v| (v.value.to_vec(), v.version)))
        }
        .await;
        self.forget_main(done).await
    }

    async fn cursor_put(
        &self,
        name: &str,
        value: Vec<u8>,
        expect: Option<u64>,
    ) -> Result<Option<u64>> {
        let condition = match expect {
            None => CacheCondition::Absent,
            Some(v) => CacheCondition::Version(v),
        };
        let done = async {
            let put = self
                .main()
                .await?
                .cache_put_if(
                    self.tenant(),
                    self.namespace(),
                    LEASE_CACHE,
                    &cursor_key(name),
                    Bytes::from(value),
                    None,
                    condition,
                )
                .await?;
            Ok(put.applied.then_some(put.version).flatten())
        }
        .await;
        self.forget_main(done).await
    }

    async fn forget(&self, member: Option<&str>) {
        match member {
            Some(m) => {
                self.members.lock().await.remove(m);
            }
            None => *self.main.lock().await = None,
        }
    }

    async fn close_idle(&self, idle: Duration) {
        self.members
            .lock()
            .await
            .retain(|_, c| c.last_used.elapsed() < idle);
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
