//! The agent's connection to the broker.

use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use crate::credential::{self, Auth};
use anyhow::{Context, Result};
use bytes::Bytes;
use felix_client::{
    AckMode, BrokerError, CacheCondition, ClientConfig, Connection, ErrorCode, GroupMember,
    GroupPollOptions, GroupRecord, quic_client_config, root_store_from_pem_file,
};

mod retry;

use retry::retrying;

/// How long the broker may take to accept the connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Where Felix is and the token to present to it.
pub(crate) struct Target {
    pub(crate) tenant: String,
    pub(crate) namespace: String,
    pub(crate) broker: String,
    pub(crate) broker_ca: PathBuf,
    pub(crate) broker_name: String,
    pub(crate) auth: Auth,
}

/// A signed-in connection to one tenant and namespace.
#[derive(Clone)]
pub(crate) struct Felix {
    // Opened at startup so a bad token or a down broker stops the server
    // before the host lists any tools. Replaced after each token renewal.
    client: Arc<RwLock<Arc<Connection>>>,
    pub(crate) tenant: String,
    pub(crate) namespace: String,
}

impl Felix {
    fn client(&self) -> Arc<Connection> {
        Arc::clone(&self.client.read().expect("client lock"))
    }
}

pub(crate) async fn connect(target: Target) -> Result<Felix> {
    let roots = root_store_from_pem_file(&target.broker_ca).with_context(|| {
        format!(
            "read the broker's certificate {}",
            target.broker_ca.display()
        )
    })?;
    let mut config =
        ClientConfig::optimized_defaults(quic_client_config(Some(Arc::new(roots)), false)?);
    config.auth_tenant_id = Some(target.tenant.clone());
    let renewer = match target.auth {
        Auth::Fixed(token) => {
            config.auth_token = Some(token);
            None
        }
        Auth::Renewing(renewer) => {
            config.token_provider = Some(renewer.clone());
            Some(renewer)
        }
    };
    // The ack carries the record's offset only once it is committed.
    config.ack_on_commit = true;
    let seeds: Vec<_> = target
        .broker
        .to_socket_addrs()
        .with_context(|| format!("resolve {}", target.broker))?
        .collect();
    let open = move || {
        let (seeds, name, config) = (seeds.clone(), target.broker.clone(), config.clone());
        let broker_name = target.broker_name.clone();
        async move {
            tokio::time::timeout(
                CONNECT_TIMEOUT,
                Connection::connect_any(&seeds, &broker_name, config),
            )
            .await
            .context("the broker did not answer in time")?
            .with_context(|| format!("connect to the broker at {name}"))
        }
    };
    let client = open().await?;
    let slot = Arc::new(RwLock::new(Arc::new(client)));
    if let Some(renewer) = renewer {
        // The streams just opened hold the token in hand. See credential.rs
        // for why a renewal needs a new connection.
        let connected_with = renewer.current().await;
        tokio::spawn(credential::reconnect_on_renewal(
            renewer,
            Arc::clone(&slot),
            connected_with,
            open,
        ));
    }
    Ok(Felix {
        client: slot,
        tenant: target.tenant,
        namespace: target.namespace,
    })
}

/// Why an extension failed.
#[derive(Debug)]
pub(crate) enum ExtendError {
    /// Felix refused with `stale_claim`: the claim lapsed or was handed out again.
    Stale(anyhow::Error),
    /// Anything else, which may pass.
    Failed(anyhow::Error),
}

impl ExtendError {
    pub(crate) fn error(&self) -> &anyhow::Error {
        match self {
            ExtendError::Stale(e) | ExtendError::Failed(e) => e,
        }
    }
}

/// What the task tools need from Felix, so their rules can be tested without
/// a broker. Streams have one shard, so every call is on shard 0.
pub(crate) trait Backend: Clone + Send + Sync + 'static {
    /// Publishes and returns the offset the record landed at.
    fn publish(&self, stream: &str, payload: Vec<u8>) -> impl Future<Output = Result<u64>> + Send;

    fn cache_put(
        &self,
        cache: &str,
        key: &str,
        value: Vec<u8>,
    ) -> impl Future<Output = Result<()>> + Send;

    fn cache_get(
        &self,
        cache: &str,
        key: &str,
    ) -> impl Future<Output = Result<Option<Vec<u8>>>> + Send;

    /// Writes only if the key has no live entry. False means it already had one.
    fn cache_put_absent(
        &self,
        cache: &str,
        key: &str,
        value: Vec<u8>,
    ) -> impl Future<Output = Result<bool>> + Send;

    /// Takes at most one record for `member`, waiting up to `wait` for one.
    fn poll(
        &self,
        stream: &str,
        group: &str,
        member: &GroupMember,
        visibility: Duration,
        wait: Duration,
    ) -> impl Future<Output = Result<Option<GroupRecord>>> + Send;

    fn ack(
        &self,
        stream: &str,
        group: &str,
        offset: u64,
    ) -> impl Future<Output = Result<()>> + Send;

    fn nack(
        &self,
        stream: &str,
        group: &str,
        record: &GroupRecord,
    ) -> impl Future<Output = Result<()>> + Send;

    fn dead_letter(
        &self,
        stream: &str,
        group: &str,
        offset: u64,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Keeps the claim standing for `by` from now and returns how long it stands.
    fn extend(
        &self,
        stream: &str,
        group: &str,
        record: &GroupRecord,
        by: Duration,
    ) -> impl Future<Output = Result<Duration, ExtendError>> + Send;
}

impl Backend for Felix {
    async fn publish(&self, stream: &str, payload: Vec<u8>) -> Result<u64> {
        let offset = retrying(|| async {
            let publisher = self.client().publisher().await?;
            publisher
                .publish(
                    &self.tenant,
                    &self.namespace,
                    stream,
                    payload.clone(),
                    AckMode::PerMessage,
                )
                .await
        })
        .await?;
        offset.context("the broker did not report an offset")
    }

    async fn cache_put(&self, cache: &str, key: &str, value: Vec<u8>) -> Result<()> {
        let value = Bytes::from(value);
        retrying(|| async {
            self.client()
                .cache_put(
                    &self.tenant,
                    &self.namespace,
                    cache,
                    key,
                    value.clone(),
                    None,
                )
                .await
        })
        .await
    }

    async fn cache_get(&self, cache: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let value = retrying(|| async {
            self.client()
                .cache_get(&self.tenant, &self.namespace, cache, key)
                .await
        })
        .await?;
        Ok(value.map(|bytes| bytes.to_vec()))
    }

    async fn cache_put_absent(&self, cache: &str, key: &str, value: Vec<u8>) -> Result<bool> {
        let value = Bytes::from(value);
        let done = retrying(|| async {
            self.client()
                .cache_put_if(
                    &self.tenant,
                    &self.namespace,
                    cache,
                    key,
                    value.clone(),
                    None,
                    CacheCondition::Absent,
                )
                .await
        })
        .await?;
        Ok(done.applied)
    }

    async fn poll(
        &self,
        stream: &str,
        group: &str,
        member: &GroupMember,
        visibility: Duration,
        wait: Duration,
    ) -> Result<Option<GroupRecord>> {
        let options = GroupPollOptions {
            member: Some(member.clone()),
            wait,
            visibility: Some(visibility),
        };
        let records = self
            .client()
            .group_poll_with(&self.tenant, &self.namespace, stream, 0, group, 1, &options)
            .await?;
        Ok(records.into_iter().next())
    }

    async fn ack(&self, stream: &str, group: &str, offset: u64) -> Result<()> {
        self.client()
            .group_ack(&self.tenant, &self.namespace, stream, 0, group, offset)
            .await
    }

    async fn nack(&self, stream: &str, group: &str, record: &GroupRecord) -> Result<()> {
        self.client()
            .group_nack(&self.tenant, &self.namespace, stream, 0, group, record)
            .await
    }

    async fn dead_letter(&self, stream: &str, group: &str, offset: u64) -> Result<()> {
        self.client()
            .group_dead_letter(&self.tenant, &self.namespace, stream, 0, group, offset)
            .await
    }

    async fn extend(
        &self,
        stream: &str,
        group: &str,
        record: &GroupRecord,
        by: Duration,
    ) -> Result<Duration, ExtendError> {
        self.client()
            .group_extend(&self.tenant, &self.namespace, stream, 0, group, record, by)
            .await
            .map_err(|e| {
                // Felix's typed refusal, not its wording, says the claim is gone.
                let stale = e
                    .downcast_ref::<BrokerError>()
                    .is_some_and(|b| b.code == ErrorCode::StaleClaim);
                if stale {
                    ExtendError::Stale(e)
                } else {
                    ExtendError::Failed(e)
                }
            })
    }
}
