//! Ingest's side of the tracker's REST API. Ingest never writes to Felix
//! itself: every record goes through the tracker under ingest's own token.

use std::future::Future;

use anyhow::{Context, Result, bail};
use kbl_core::names::ItemId;
use kbl_core::work::{Actor, WorkRecord};
use serde::Deserialize;
use serde_json::{Value, json};

/// An imported root, as the tracker folds it.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Root {
    pub(crate) item: ItemId,
    pub(crate) source: String,
    pub(crate) upstream_id: String,
    pub(crate) stage: String,
    pub(crate) closed: bool,
    /// Upstream sync wrote the close, not a person. Only such a root is
    /// reopened when its issue is.
    #[serde(default)]
    pub(crate) closed_by_sync: bool,
    /// Who wrote the last reopen, if anyone did. An open root whose issue is
    /// closed is a person's choice only when a person reopened it.
    #[serde(default)]
    pub(crate) reopened_by: Option<Actor>,
    /// It has its triage and augment items. A root back in Triage that has
    /// them was reopened, so a person, not ingest, accepts it again.
    #[serde(default)]
    pub(crate) has_children: bool,
    /// The agent's pull request, `owner/repo#number`.
    pub(crate) pr: Option<String>,
    /// When the item was first claimed, in milliseconds since the epoch.
    #[serde(default)]
    pub(crate) claimed_at_ms: Option<u64>,
    /// What the fold holds for `pr`, which differs from what ingest last
    /// wrote when the agent changed `pr` in between.
    #[serde(default)]
    pub(crate) pr_status: Option<kbl_core::work::PrSnapshot>,
}

/// What the tracker did with a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Appended {
    Written,
    /// The fold refused it, for this reason. `duplicate` means it was already there.
    Refused(String),
}

/// Saved state and the version it was read at.
pub(crate) type Saved = (Option<Value>, Option<u64>);

pub(crate) trait Tracker: Send + Sync {
    fn roots(&self) -> impl Future<Output = Result<Vec<Root>>> + Send;

    fn append(&self, record: &WorkRecord) -> impl Future<Output = Result<Appended>> + Send;

    fn load(&self, source: &str) -> impl Future<Output = Result<Saved>> + Send;

    /// Saves if the version is still `version`; returns the new one.
    fn save(
        &self,
        source: &str,
        state: &Value,
        version: Option<u64>,
    ) -> impl Future<Output = Result<u64>> + Send;
}

pub(crate) struct Http {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl Http {
    pub(crate) fn new(base: &str, token: String) -> Result<Self> {
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            token,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
        })
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<(u16, Value)> {
        let res = req
            .bearer_auth(&self.token)
            .send()
            .await
            .context("reach the tracker")?;
        let status = res.status().as_u16();
        let body = res.json().await.unwrap_or(Value::Null);
        Ok((status, body))
    }
}

impl Tracker for Http {
    async fn roots(&self) -> Result<Vec<Root>> {
        let (status, body) = self
            .send(self.client.get(format!("{}/v1/ingest/roots", self.base)))
            .await?;
        if status != 200 {
            bail!("the tracker answered {status} to the roots call: {body}");
        }
        Ok(serde_json::from_value(body["roots"].clone())?)
    }

    async fn append(&self, record: &WorkRecord) -> Result<Appended> {
        let (status, body) = self
            .send(
                self.client
                    .post(format!("{}/v1/ingest/records", self.base))
                    .json(&json!({ "record": record })),
            )
            .await?;
        match status {
            200 => Ok(Appended::Written),
            409 => Ok(Appended::Refused(
                body["why"].as_str().unwrap_or("invalid").to_owned(),
            )),
            _ => bail!(
                "the tracker answered {status} to a {} record: {body}",
                record.kind()
            ),
        }
    }

    async fn load(&self, source: &str) -> Result<Saved> {
        let (status, body) = self
            .send(
                self.client
                    .get(format!("{}/v1/ingest/state/{source}", self.base)),
            )
            .await?;
        if status != 200 {
            bail!("the tracker answered {status} to the state call: {body}");
        }
        let state = (!body["state"].is_null()).then(|| body["state"].clone());
        Ok((state, body["version"].as_u64()))
    }

    async fn save(&self, source: &str, state: &Value, version: Option<u64>) -> Result<u64> {
        let (status, body) = self
            .send(
                self.client
                    .put(format!("{}/v1/ingest/state/{source}", self.base))
                    .json(&json!({ "state": state, "version": version })),
            )
            .await?;
        match (status, body["version"].as_u64()) {
            (200, Some(v)) => Ok(v),
            _ => bail!("could not save the state: {status}: {body}"),
        }
    }
}
