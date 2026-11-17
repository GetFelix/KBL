//! The HTTP under the GitLab adapter. The token is read here and goes
//! nowhere else: not into the config, the state, a record, an error or a log
//! line.

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::header::HeaderValue;
use serde_json::Value;

use crate::ado::transport::Secret;
use crate::github::{Req, Res, Transport};

pub(crate) struct Reqwest {
    client: reqwest::Client,
    token: Secret,
}

impl Reqwest {
    pub(crate) fn new(token: Secret) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                // A redirect would carry the token to wherever it points.
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .user_agent("kbl-ingest")
                .build()?,
            token,
        })
    }
}

impl Transport for Reqwest {
    async fn send(&self, req: Req) -> Result<Res> {
        // Read for every request, so a rotated token is picked up.
        let token = self.token.read("GitLab token")?;
        // The reason is fixed text: the library's own message could quote the value.
        let mut value = HeaderValue::from_str(&token)
            .map_err(|_| anyhow::anyhow!("the GitLab token is not a valid header value"))?;
        value.set_sensitive(true);
        let method = reqwest::Method::from_bytes(req.method.as_bytes())?;
        let mut b = self
            .client
            .request(method, &req.url)
            .header("private-token", value)
            .header("accept", "application/json");
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
