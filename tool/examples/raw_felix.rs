//! A broker client that goes around the tracker, for `dev/check-teams.mjs`.
//! It shows what a token can reach on Felix itself. Not shipped.
//!
//! ```text
//! FELIX_TOKEN=... raw_felix read <namespace> <stream>
//! FELIX_TOKEN=... raw_felix publish <namespace> <stream> <payload>
//! ```

use std::net::ToSocketAddrs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use felix_client::{AckMode, Client, ClientConfig, quic_client_config, root_store_from_pem_file};

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("{err:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let tenant = env("KBL_TENANT", "agents");
    let token = std::env::var("FELIX_TOKEN").context("FELIX_TOKEN is not set")?;
    let broker = env("KBL_FELIX_BROKER", "127.0.0.1:5000");
    let roots = root_store_from_pem_file(Path::new(&env(
        "KBL_FELIX_CA_FILE",
        "dev/state/broker-cert.pem",
    )))?;
    let mut config =
        ClientConfig::optimized_defaults(quic_client_config(Some(Arc::new(roots)), false)?);
    config.auth_tenant_id = Some(tenant.clone());
    config.auth_token = Some(token);
    config.ack_on_commit = true;
    let seeds: Vec<_> = broker.to_socket_addrs()?.collect();
    let client =
        Client::connect_any(&seeds, &env("KBL_FELIX_BROKER_NAME", "localhost"), config).await?;

    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["read", namespace, stream] => {
            let mut from = 0;
            loop {
                let page = client
                    .read(&tenant, namespace, stream, 0, from, None, 0)
                    .await?;
                if page.records.is_empty() {
                    return Ok(());
                }
                for record in &page.records {
                    println!("{}", String::from_utf8_lossy(&record.payload));
                }
                if page.next_offset <= from {
                    bail!("reading {stream} did not advance past offset {from}");
                }
                from = page.next_offset;
            }
        }
        ["publish", namespace, stream, payload] => {
            let publisher = client.publisher().await?;
            let offset = publisher
                .publish(
                    &tenant,
                    namespace,
                    stream,
                    payload.as_bytes().to_vec(),
                    AckMode::PerMessage,
                )
                .await?;
            println!("{}", offset.context("the broker did not report an offset")?);
            Ok(())
        }
        _ => bail!(
            "usage: raw_felix read <namespace> <stream> | publish <namespace> <stream> <payload>"
        ),
    }
}
