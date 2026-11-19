//! The tracker's connection to Felix: sign in, create its streams, and the
//! few broker calls the claim path needs.

use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
pub(crate) use felix_client::ConditionalWrite;
use felix_client::{
    AckMode, BrokerError, CacheCondition, ClientConfig, Connection, CursorErrorReason, ErrorCode,
    GroupMember, GroupPollOptions, GroupRecord, RetryClass, StartPosition, SubscribeCursorError,
    quic_client_config, root_store_from_pem_file,
};
use kbl_core::org::{ORG_EVENTS, ORG_TRACKER_CACHE};
use kbl_core::work::{LANES, READY_GROUP, WORK_EVENTS, ready_stream};
use serde_json::{Value, json};

/// The cache and key that hold a team's writer lease.
const LEASE_CACHE: &str = "work.tracker";
const LEASE_KEY: &str = "writer";

/// A lease holder name no other tracker can have: a tracker that reads the
/// lease entry back takes it as its own only when the bytes match.
pub(crate) fn holder_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("drawing a lease holder name: {e}"))?;
    let nonce: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("tracker-{}-{nonce}", std::process::id()))
}

/// What a connection is for: one team's namespace, or the org's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Team,
    Org,
}

/// The key ingest's state for source `name` is kept under, in the lease cache.
fn cursor_key(name: &str) -> String {
    format!("ingest-{name}")
}

/// The key of a source's ingest lease. A source id holds `:` and `/` and a
/// state name is a key, which has neither, so it never meets [`cursor_key`].
fn source_lease_key(source: &str) -> String {
    format!("ingest/lease/{source}")
}

/// The key of a worker group's live members. A group is a key, with no `:`,
/// so it never meets [`source_lease_key`].
fn group_members_key(group: &str) -> String {
    format!("ingest/members/{group}")
}

/// The key of a budget pool's shared backoff, in the org's cache. A pool is a
/// key, so it never meets the keys above.
fn pool_backoff_key(pool: &str) -> String {
    format!("ingest/budget/{pool}")
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

/// One page of a stream, and the offset after it (none at the tip).
pub(crate) type Page = (Vec<(u64, Vec<u8>)>, Option<u64>);

/// Records read, and the committed tip after them.
pub(crate) type ToTip = (Vec<(u64, Vec<u8>)>, u64);

/// What the claim path needs from Felix, so its rules can be tested without a
/// broker. Every stream has one shard.
pub(crate) trait Broker: Clone + Send + Sync + 'static {
    /// Publishes and returns the offset the record landed at.
    fn publish(&self, stream: &str, payload: Vec<u8>) -> impl Future<Output = Result<u64>> + Send;

    /// Publishes only if the record would land at `expected`, the stream's
    /// next offset: nothing was appended since the caller read its tail.
    /// Felix makes the check where it assigns offsets, so a writer that lost
    /// its lease cannot append behind a newer writer's back.
    fn publish_if(
        &self,
        stream: &str,
        payload: Vec<u8>,
        expected: u64,
    ) -> impl Future<Output = Result<ConditionalWrite>> + Send;

    /// Every committed record from `from`, oldest first. Offsets can skip.
    fn read_from(
        &self,
        stream: &str,
        from: u64,
    ) -> impl Future<Output = Result<Vec<(u64, Vec<u8>)>>> + Send;

    /// Every committed record from `from`, and the offset after the last
    /// committed one. That tip can pass the last record read: offsets that
    /// hold no record, like a shard leader's generation start, are skipped.
    /// It never counts an offset claimed but not yet committed.
    fn read_to_tip(&self, stream: &str, from: u64) -> impl Future<Output = Result<ToTip>> + Send {
        async move {
            let records = self.read_from(stream, from).await?;
            let tip = records.last().map_or(from, |(o, _)| o + 1);
            Ok((records, tip))
        }
    }

    /// One page of committed records from `from`, oldest first, and the
    /// offset to read next, or none at the tip. A caller that stops early
    /// does not read the rest of the stream.
    fn read_page(&self, stream: &str, from: u64) -> impl Future<Output = Result<Page>> + Send {
        async move { Ok((self.read_from(stream, from).await?, None)) }
    }

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

    /// The writer lease key's live entry: its holder and version.
    fn lease_get(&self) -> impl Future<Output = Result<Option<(Vec<u8>, u64)>>> + Send;

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

    /// A budget pool's shared backoff and its version.
    fn pool_backoff_get(
        &self,
        _pool: &str,
    ) -> impl Future<Output = Result<Option<(Vec<u8>, u64)>>> + Send {
        async { bail!("this broker keeps no pool backoffs") }
    }

    /// Writes a pool's backoff if its version is still `expect` (none:
    /// absent). Returns the new version, or none when the condition failed.
    fn pool_backoff_put(
        &self,
        _pool: &str,
        _value: Vec<u8>,
        _expect: Option<u64>,
    ) -> impl Future<Output = Result<Option<u64>>> + Send {
        async { bail!("this broker keeps no pool backoffs") }
    }

    /// A source's ingest lease and its version.
    fn source_lease_get(
        &self,
        _source: &str,
    ) -> impl Future<Output = Result<Option<(Vec<u8>, u64)>>> + Send {
        async { bail!("this broker keeps no ingest leases") }
    }

    /// Writes a source's ingest lease if its version is still `expect` (none:
    /// absent). Returns the new version, or none when the condition failed.
    fn source_lease_put(
        &self,
        _source: &str,
        _value: Vec<u8>,
        _expect: Option<u64>,
    ) -> impl Future<Output = Result<Option<u64>>> + Send {
        async { bail!("this broker keeps no ingest leases") }
    }

    /// A worker group's live members and their version.
    fn group_members_get(
        &self,
        _group: &str,
    ) -> impl Future<Output = Result<Option<(Vec<u8>, u64)>>> + Send {
        async { bail!("this broker keeps no ingest leases") }
    }

    /// Writes a worker group's members if their version is still `expect`.
    fn group_members_put(
        &self,
        _group: &str,
        _value: Vec<u8>,
        _expect: Option<u64>,
    ) -> impl Future<Output = Result<Option<u64>>> + Send {
        async { bail!("this broker keeps no ingest leases") }
    }

    /// Fails when `work.events` is no longer acknowledged durably. Checked on
    /// every rebuild, because the stream's settings can change or the stream
    /// be recreated while the tracker runs.
    fn check_acks(&self) -> impl Future<Output = Result<()>> + Send {
        async { Ok(()) }
    }

    /// Drops a connection that failed or timed out, so the next call opens a
    /// new one: `member`'s, or the log's when none.
    fn forget(&self, member: Option<&str>) -> impl Future<Output = ()> + Send;

    /// Closes the connections of agents that have made no call for `idle`.
    fn close_idle(&self, idle: Duration) -> impl Future<Output = ()> + Send;
}

/// A connection to the broker and when it was last used.
struct Conn {
    client: Arc<Connection>,
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
        // Narrowed to this namespace, so a record routed to the wrong team's
        // connection is refused by Felix, not only by this code.
        let request = self
            .http
            .post(format!(
                "{}/v1/tenants/{}/token/exchange",
                self.target.control_plane, self.target.tenant
            ))
            .bearer_auth(&id_token)
            .json(&json!({
                "audience": audience,
                "resources": [format!("namespace:{}/{}", self.target.tenant, self.target.namespace)],
            }));
        let body: Value = send(request, "exchange the identity token")
            .await?
            .json()
            .await?;
        string(&body, "felix_token")
    }

    /// A new connection with a freshly exchanged token.
    async fn open(&self) -> Result<Arc<Connection>> {
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
            Connection::connect_any(&self.seeds, &self.target.broker_name, config),
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
    lease_cache: &'static str,
    kind: Kind,
}

/// Signs in, creates the streams, cache and consumer groups of a team (or of
/// the org) if they are not there yet, and returns the broker connection.
pub(crate) async fn connect(target: Target, kind: Kind) -> Result<Felix> {
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

    settle("create the tracker's streams", || async {
        let admin = session.exchange("felix-controlplane").await?;
        let url = format!(
            "{}/v1/tenants/{}/namespaces/{}/resources",
            session.target.control_plane, session.target.tenant, session.target.namespace
        );
        send(
            session
                .http
                .post(url)
                .bearer_auth(&admin)
                .json(&streams_batch(kind)),
            "create the tracker's streams",
        )
        .await
        .map(drop)
    })
    .await?;
    if kind == Kind::Team {
        work_events_acks(&session).await?;
    }

    let felix = Felix {
        kind,
        session,
        main: Arc::default(),
        members: Arc::default(),
        lease_cache: match kind {
            Kind::Team => LEASE_CACHE,
            Kind::Org => ORG_TRACKER_CACHE,
        },
    };
    if kind == Kind::Team {
        for lane in 0..LANES {
            let stream = ready_stream(lane);
            settle("create the ready groups", || felix.ensure_group(&stream)).await?;
        }
    }
    Ok(felix)
}

/// Reads `work.events` from the control plane and refuses it unless its acks
/// are durable.
async fn work_events_acks(session: &Session) -> Result<()> {
    let admin = session.exchange("felix-controlplane").await?;
    let url = format!(
        "{}/v1/tenants/{}/namespaces/{}/streams/{WORK_EVENTS}",
        session.target.control_plane, session.target.tenant, session.target.namespace
    );
    let stream: Value = send(
        session.http.get(url).bearer_auth(&admin),
        "read how work.events is acknowledged",
    )
    .await?
    .json()
    .await?;
    acks_are_durable(&stream)
}

/// Refuses a `work.events` whose acknowledged records a failover could cut.
/// The claim path trusts an acknowledged record to stay at its offset: a cut
/// record whose offset another writer refills reads as the same log. That
/// holds for a durable stream acknowledged by a quorum, and for a durable
/// stream with one copy, which Felix holds unplaced while its broker is down
/// rather than failing it over. The control plane does not show a shard's
/// replication engine, so a stream on Raft is held to the same settings.
fn acks_are_durable(stream: &Value) -> Result<()> {
    let durable = stream["durable"].as_bool() == Some(true);
    let quorum = stream["consistency"].as_str() == Some("Quorum");
    // A missing count is not one copy.
    let copies = stream["replication_factor"].as_u64();
    if durable && (quorum || copies == Some(1)) {
        return Ok(());
    }
    let copies = copies.map_or("unknown".to_owned(), |c| c.to_string());
    bail!(
        "{WORK_EVENTS} is acknowledged before a quorum holds it (durable: {durable}, \
         consistency: {}, replication_factor: {copies}), so a failover could drop a \
         record the tracker was told was written. Make it durable and either \
         `Quorum` or one copy; see docs/work-tracker.md, \"What Felix must guarantee\"",
        stream["consistency"].as_str().unwrap_or("unknown"),
    )
}

/// A team's log and ready lanes, one durable stream each, and its lease
/// cache; or the org's log and lease cache.
fn streams_batch(kind: Kind) -> Value {
    let (names, cache): (Vec<String>, &str) = match kind {
        Kind::Team => (
            std::iter::once(WORK_EVENTS.to_owned())
                .chain((0..LANES).map(ready_stream))
                .collect(),
            LEASE_CACHE,
        ),
        Kind::Org => (vec![ORG_EVENTS.to_owned()], ORG_TRACKER_CACHE),
    };
    let streams: Vec<Value> = names
        .into_iter()
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
        "cache": cache,
        "display_name": cache,
        "shards": 1,
        "replication_factor": 1,
        "consistency": "Leader",
    });
    json!({ "streams": streams, "caches": [lease] })
}

impl Felix {
    /// A value in the lease cache and its version.
    async fn cache_get_key(&self, key: &str) -> Result<Option<(Vec<u8>, u64)>> {
        let done = async {
            let client = self.main().await?;
            let got = placed(|| {
                client.cache_get_versioned(self.tenant(), self.namespace(), self.lease_cache, key)
            })
            .await?;
            Ok(got.map(|v| (v.value.to_vec(), v.version)))
        }
        .await;
        self.forget_main(done).await
    }

    /// Writes `key` in the lease cache if its version is still `expect`.
    async fn cache_put_key(
        &self,
        key: &str,
        value: Vec<u8>,
        expect: Option<u64>,
    ) -> Result<Option<u64>> {
        let condition = match expect {
            None => CacheCondition::Absent,
            Some(v) => CacheCondition::Version(v),
        };
        let done = async {
            let client = self.main().await?;
            let body = Bytes::from(value);
            let put = placed(|| {
                client.cache_put_if(
                    self.tenant(),
                    self.namespace(),
                    self.lease_cache,
                    key,
                    body.clone(),
                    None,
                    condition,
                )
            })
            .await?;
            Ok(put.applied.then_some(put.version).flatten())
        }
        .await;
        self.forget_main(done).await
    }

    fn tenant(&self) -> &str {
        &self.session.target.tenant
    }

    fn namespace(&self) -> &str {
        &self.session.target.namespace
    }

    /// The log connection, opened if there is none.
    async fn main(&self) -> Result<Arc<Connection>> {
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
    async fn member(&self, member: &str) -> Result<Arc<Connection>> {
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

    async fn publish_if(
        &self,
        stream: &str,
        payload: Vec<u8>,
        expected: u64,
    ) -> Result<ConditionalWrite> {
        let done = async {
            let client = self.main().await?;
            placed(|| {
                client.publish_if(
                    self.tenant(),
                    self.namespace(),
                    stream,
                    None,
                    vec![payload.clone()],
                    expected,
                )
            })
            .await
            .with_context(|| format!("publish to {stream}"))
        }
        .await;
        self.forget_main(done).await
    }

    async fn read_from(&self, stream: &str, from: u64) -> Result<Vec<(u64, Vec<u8>)>> {
        Ok(self.read_to_tip(stream, from).await?.0)
    }

    async fn read_to_tip(&self, stream: &str, from: u64) -> Result<ToTip> {
        let done = async {
            let client = self.main().await?;
            let client = &client;
            pages_to_tip(stream, from, |from| async move {
                let page = placed(|| {
                    client.read(self.tenant(), self.namespace(), stream, 0, from, None, 0)
                })
                .await?;
                let records = page
                    .records
                    .into_iter()
                    .map(|r| (r.offset, r.payload.to_vec()))
                    .collect();
                Ok((records, page.next_offset))
            })
            .await
        }
        .await;
        self.forget_main(done).await
    }

    async fn read_page(&self, stream: &str, from: u64) -> Result<Page> {
        let done = async {
            let client = self.main().await?;
            let page = match placed(|| {
                client.read(self.tenant(), self.namespace(), stream, 0, from, None, 0)
            })
            .await
            {
                Ok(page) => page,
                // Felix refuses an offset past its tail; nothing is there yet,
                // and the connection is fine.
                Err(e) if past_the_tail(&e) => return Ok((Vec::new(), None)),
                Err(e) => return Err(e.context(format!("read {stream} from offset {from}"))),
            };
            if page.records.is_empty() {
                return Ok((Vec::new(), None));
            }
            anyhow::ensure!(
                page.next_offset > from,
                "reading {stream} did not advance past offset {from}"
            );
            let records = page
                .records
                .into_iter()
                .map(|r| (r.offset, r.payload.to_vec()))
                .collect();
            Ok((records, Some(page.next_offset)))
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
            let client = self.main().await?;
            let body = Bytes::from(holder);
            let put = placed(|| {
                client.cache_put_if(
                    self.tenant(),
                    self.namespace(),
                    self.lease_cache,
                    LEASE_KEY,
                    body.clone(),
                    Some(ttl.as_millis() as u64),
                    condition,
                )
            })
            .await?;
            Ok(put.applied.then_some(put.version).flatten())
        }
        .await;
        self.forget_main(done).await
    }

    async fn lease_get(&self) -> Result<Option<(Vec<u8>, u64)>> {
        let done = async {
            let client = self.main().await?;
            let got = placed(|| {
                client.cache_get_versioned(
                    self.tenant(),
                    self.namespace(),
                    self.lease_cache,
                    LEASE_KEY,
                )
            })
            .await?;
            Ok(got.map(|v| (v.value.to_vec(), v.version)))
        }
        .await;
        self.forget_main(done).await
    }

    async fn source_lease_get(&self, source: &str) -> Result<Option<(Vec<u8>, u64)>> {
        self.cache_get_key(&source_lease_key(source)).await
    }

    async fn source_lease_put(
        &self,
        source: &str,
        value: Vec<u8>,
        expect: Option<u64>,
    ) -> Result<Option<u64>> {
        self.cache_put_key(&source_lease_key(source), value, expect)
            .await
    }

    async fn group_members_get(&self, group: &str) -> Result<Option<(Vec<u8>, u64)>> {
        self.cache_get_key(&group_members_key(group)).await
    }

    async fn group_members_put(
        &self,
        group: &str,
        value: Vec<u8>,
        expect: Option<u64>,
    ) -> Result<Option<u64>> {
        self.cache_put_key(&group_members_key(group), value, expect)
            .await
    }

    async fn pool_backoff_get(&self, pool: &str) -> Result<Option<(Vec<u8>, u64)>> {
        self.cache_get_key(&pool_backoff_key(pool)).await
    }

    async fn pool_backoff_put(
        &self,
        pool: &str,
        value: Vec<u8>,
        expect: Option<u64>,
    ) -> Result<Option<u64>> {
        self.cache_put_key(&pool_backoff_key(pool), value, expect)
            .await
    }

    async fn cursor_get(&self, name: &str) -> Result<Option<(Vec<u8>, u64)>> {
        self.cache_get_key(&cursor_key(name)).await
    }

    async fn cursor_put(
        &self,
        name: &str,
        value: Vec<u8>,
        expect: Option<u64>,
    ) -> Result<Option<u64>> {
        self.cache_put_key(&cursor_key(name), value, expect).await
    }

    async fn check_acks(&self) -> Result<()> {
        match self.kind {
            Kind::Team => work_events_acks(&self.session).await,
            Kind::Org => Ok(()),
        }
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

/// How long a missing stream or cache is retried, as felix-client does.
const NOT_FOUND_GRACE: Duration = Duration::from_secs(5);

/// What a broker says while a new stream's shard has no owner yet or is still opening.
const NOT_READY: [&str; 2] = ["not assigned", "still opening"];

/// How long startup keeps retrying while a fresh broker settles its leases.
const SETTLE_WAIT: Duration = Duration::from_secs(120);

/// Runs a startup call until it succeeds or `SETTLE_WAIT` passes, backing off
/// between tries. A freshly started broker refuses reads until its shard lease
/// settles, and exiting would only hand the retry to the container engine.
/// Only idempotent calls go through here.
async fn settle<T, F, Fut>(doing: &str, op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    settle_with(doing, op, SETTLE_WAIT, Duration::from_millis(250)).await
}

async fn settle_with<T, F, Fut>(
    doing: &str,
    mut op: F,
    wait: Duration,
    first_delay: Duration,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let deadline = Instant::now() + wait;
    let mut delay = first_delay;
    loop {
        match op().await {
            Err(err) if Instant::now() < deadline && err.downcast_ref::<Refused>().is_none() => {
                eprintln!("kbl-tracker: {doing} failed, retrying in {delay:?}: {err:#}");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
            done => return done,
        }
    }
}

/// Runs `op`, trying again while the broker says the shard is not ready or is
/// moving (a refusal Felix classes as retry or retry_after, which it sends
/// before applying anything). No other failure is retried: a second publish
/// could land twice, and an unknown outcome could hide an applied lease write.
async fn placed<T, F, Fut>(mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    use tokio::time::Instant;
    let deadline = Instant::now() + PLACEMENT_WAIT;
    let mut missing_since: Option<Instant> = None;
    loop {
        match op().await {
            // A missing stream or cache is usually real, so it gets a short grace.
            Err(err) if is_not_found(&err) => {
                let since = *missing_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= NOT_FOUND_GRACE || Instant::now() >= deadline {
                    return Err(err);
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(err) if Instant::now() < deadline && is_not_ready(&err) => {
                let asked = err
                    .downcast_ref::<BrokerError>()
                    .and_then(|b| b.detail.as_ref())
                    .and_then(|d| d.retry_after_ms)
                    .map(Duration::from_millis)
                    .unwrap_or_default();
                tokio::time::sleep(asked.clamp(Duration::from_millis(250), Duration::from_secs(5)))
                    .await;
            }
            done => return done,
        }
    }
}

/// Whether Felix refused a read because the offset is past the stream's tail.
/// Reads pages from `from` until an empty one: the records, and the committed
/// tip. `read` returns a page's records and the offset after it. The empty
/// page's offset can pass the last record, when a new shard leader's
/// generation start took the offsets after it.
async fn pages_to_tip<F, Fut>(stream: &str, from: u64, mut read: F) -> Result<ToTip>
where
    F: FnMut(u64) -> Fut,
    Fut: Future<Output = Result<(Vec<(u64, Vec<u8>)>, u64)>>,
{
    let mut out = Vec::new();
    let mut from = from;
    loop {
        let (records, next) = match read(from).await {
            Ok(page) => page,
            Err(e) if past_the_tail(&e) => return Ok((out, from)),
            Err(e) => return Err(e.context(format!("read {stream} from offset {from}"))),
        };
        if records.is_empty() {
            return Ok((out, from.max(next)));
        }
        out.extend(records);
        anyhow::ensure!(
            next > from,
            "reading {stream} did not advance past offset {from}"
        );
        from = next;
    }
}

fn past_the_tail(err: &anyhow::Error) -> bool {
    err.downcast_ref::<SubscribeCursorError>()
        .is_some_and(|c| c.reason == CursorErrorReason::InFuture)
}

fn is_not_found(err: &anyhow::Error) -> bool {
    err.downcast_ref::<BrokerError>().is_some_and(|b| {
        b.code == ErrorCode::NotFound
            && matches!(b.retry, RetryClass::Retry | RetryClass::RetryAfter)
    })
}

fn is_not_ready(err: &anyhow::Error) -> bool {
    if let Some(broker) = err.downcast_ref::<BrokerError>()
        && matches!(broker.retry, RetryClass::Retry | RetryClass::RetryAfter)
    {
        return true;
    }
    // An older broker sends no class, only the words.
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
    let message = format!("could not {doing}: {status}: {text}");
    if status.is_client_error() {
        return Err(anyhow::Error::new(Refused(message)));
    }
    bail!("{message}")
}

/// A request the control plane or IdP refused as wrong (a 4xx). Waiting does
/// not change the answer, so startup does not retry it.
#[derive(Debug)]
struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn string(body: &Value, field: &str) -> Result<String> {
    body[field]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("the reply has no `{field}`"))
}

#[cfg(test)]
mod tests;
