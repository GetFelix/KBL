//! The agent's connection to the broker.

use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use felix_client::{Client, ClientConfig, quic_client_config, root_store_from_pem_file};

/// How long the broker may take to accept the connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Where Felix is and the token to present to it.
pub(crate) struct Target {
    pub(crate) tenant: String,
    pub(crate) namespace: String,
    pub(crate) broker: String,
    pub(crate) broker_ca: PathBuf,
    pub(crate) broker_name: String,
    pub(crate) token: String,
}

/// A signed-in connection to one tenant and namespace.
#[derive(Clone)]
pub(crate) struct Felix {
    // The task tools use this. It is opened at startup so a bad token or a
    // down broker stops the server before the host lists any tools.
    #[expect(dead_code, reason = "used by the task tools")]
    client: Arc<Client>,
    pub(crate) tenant: String,
    pub(crate) namespace: String,
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
    config.auth_token = Some(target.token);
    // The ack carries the record's offset only once it is committed.
    config.ack_on_commit = true;
    let seeds: Vec<_> = target
        .broker
        .to_socket_addrs()
        .with_context(|| format!("resolve {}", target.broker))?
        .collect();
    let client = tokio::time::timeout(
        CONNECT_TIMEOUT,
        Client::connect_any(&seeds, &target.broker_name, config),
    )
    .await
    .context("the broker did not answer in time")?
    .with_context(|| format!("connect to the broker at {}", target.broker))?;
    Ok(Felix {
        client: Arc::new(client),
        tenant: target.tenant,
        namespace: target.namespace,
    })
}
