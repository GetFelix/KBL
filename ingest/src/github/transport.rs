//! The HTTP under the GitHub adapter, as a trait so tests run it against
//! recorded answers and never reach GitHub.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) struct Req {
    pub(crate) method: &'static str,
    pub(crate) url: String,
    pub(crate) if_none_match: Option<String>,
    pub(crate) body: Option<Value>,
}

impl Req {
    pub(crate) fn get(url: String) -> Self {
        Self {
            method: "GET",
            url,
            if_none_match: None,
            body: None,
        }
    }

    pub(crate) fn post(url: String, body: Value) -> Self {
        Self {
            method: "POST",
            url,
            if_none_match: None,
            body: Some(body),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Res {
    pub(crate) status: u16,
    /// Header names in lower case.
    pub(crate) headers: BTreeMap<String, String>,
    /// Null when there is no JSON body.
    pub(crate) body: Value,
}

pub(crate) trait Transport: Send + Sync {
    fn send(&self, req: Req) -> impl Future<Output = Result<Res>> + Send;
}

/// Where the token comes from.
pub(crate) enum Token {
    File(PathBuf),
    Env(String),
}

impl Token {
    /// Read for every request, so a refreshed App installation token is picked up.
    fn read(&self) -> Result<String> {
        let token = match self {
            Token::File(p) => std::fs::read_to_string(p)
                .with_context(|| format!("read the GitHub token file {}", p.display()))?,
            Token::Env(name) => {
                std::env::var(name).with_context(|| format!("{name} is not set"))?
            }
        };
        let token = token.trim().to_owned();
        if token.is_empty() {
            bail!("the GitHub token is empty");
        }
        Ok(token)
    }
}

pub(crate) struct Reqwest {
    client: reqwest::Client,
    token: Token,
}

impl Reqwest {
    pub(crate) fn new(token: Token) -> Result<Self> {
        Ok(Self {
            // A transferred issue answers with a redirect we want to see.
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .user_agent("agents-ingest")
                .build()?,
            token,
        })
    }
}

impl Transport for Reqwest {
    async fn send(&self, req: Req) -> Result<Res> {
        let method = reqwest::Method::from_bytes(req.method.as_bytes())?;
        let mut b = self
            .client
            .request(method, &req.url)
            .bearer_auth(self.token.read()?)
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28");
        if let Some(tag) = &req.if_none_match {
            b = b.header("if-none-match", tag);
        }
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
