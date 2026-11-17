//! The HTTP under the Jira adapter. The credential is read here and goes
//! nowhere else: not into the config, the state or any record.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::github::{Req, Res, Transport};

/// Where a secret comes from, read again for every request so a refreshed
/// OAuth token is picked up.
pub(crate) enum Secret {
    Env(String),
    File(PathBuf),
}

impl Secret {
    fn read(&self) -> Result<String> {
        let text = match self {
            Secret::Env(name) => {
                std::env::var(name).with_context(|| format!("{name} is not set"))?
            }
            Secret::File(p) => std::fs::read_to_string(p)
                .with_context(|| format!("read the Jira credential file {}", p.display()))?,
        };
        let text = text.trim().to_owned();
        if text.is_empty() {
            bail!("the Jira credential is empty");
        }
        Ok(text)
    }
}

pub(crate) enum Auth {
    /// An API token: HTTP basic with the account email.
    Token { email: String, secret: Secret },
    /// An OAuth app's access token, as a bearer.
    Bearer(Secret),
}

pub(crate) struct Reqwest {
    client: reqwest::Client,
    auth: Auth,
}

impl Reqwest {
    pub(crate) fn new(auth: Auth) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .user_agent("kbl-ingest")
                .build()?,
            auth,
        })
    }
}

impl Transport for Reqwest {
    async fn send(&self, req: Req) -> Result<Res> {
        let method = reqwest::Method::from_bytes(req.method.as_bytes())?;
        let mut b = self
            .client
            .request(method, &req.url)
            .header("accept", "application/json");
        b = match &self.auth {
            Auth::Token { email, secret } => b.basic_auth(email, Some(secret.read()?)),
            Auth::Bearer(secret) => b.bearer_auth(secret.read()?),
        };
        if let Some(body) = &req.body {
            b = b.json(body);
        }
        let res = b
            .send()
            .await
            .with_context(|| format!("reach {}", req.url))?;
        let status = res.status().as_u16();
        let headers = res
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned())))
            .collect();
        let body = res.json().await.unwrap_or(Value::Null);
        Ok(Res {
            status,
            headers,
            body,
        })
    }
}
