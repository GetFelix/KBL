//! The HTTP under the Azure DevOps adapter. The credential is read here and
//! goes nowhere else: not into the config, the state, a record, an error or
//! a log line.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::github::{Req, Res, Transport};

/// The Azure DevOps resource, as an Entra scope.
const ENTRA_SCOPE: &str = "499b84ac-1321-427f-aa17-267ca6975798/.default";
/// A token is replaced this long before it expires.
const EARLY: Duration = Duration::from_secs(120);

/// Where a secret comes from, read again for every use so a rotated one is
/// picked up.
pub(crate) enum Secret {
    Env(String),
    File(PathBuf),
}

impl Secret {
    pub(crate) fn read(&self, what: &str) -> Result<String> {
        let text = match self {
            Secret::Env(name) => {
                std::env::var(name).with_context(|| format!("{name} is not set"))?
            }
            Secret::File(p) => std::fs::read_to_string(p)
                .with_context(|| format!("read the {what} file {}", p.display()))?,
        };
        let text = text.trim().to_owned();
        if text.is_empty() {
            bail!("the {what} is empty");
        }
        Ok(text)
    }
}

/// A Microsoft Entra service principal, signed in with client credentials.
pub(crate) struct Entra {
    pub(crate) tenant: String,
    pub(crate) client_id: String,
    pub(crate) secret: Secret,
    pub(crate) authority: String,
    cache: Mutex<Option<(String, Instant)>>,
}

impl Entra {
    pub(crate) fn new(tenant: &str, client_id: &str, secret: Secret, authority: &str) -> Self {
        Self {
            tenant: tenant.to_owned(),
            client_id: client_id.to_owned(),
            secret,
            authority: authority.trim_end_matches('/').to_owned(),
            cache: Mutex::new(None),
        }
    }

    async fn token(&self, client: &reqwest::Client) -> Result<String> {
        let mut cache = self.cache.lock().await;
        if let Some((t, until)) = &*cache
            && *until > Instant::now()
        {
            return Ok(t.clone());
        }
        let secret = self.secret.read("Entra client secret")?;
        let url = format!("{}/{}/oauth2/v2.0/token", self.authority, self.tenant);
        let res = client
            .post(&url)
            .form(&[
                ("grant_type", "client_credentials"),
                ("client_id", self.client_id.as_str()),
                ("client_secret", secret.as_str()),
                ("scope", ENTRA_SCOPE),
            ])
            .send()
            .await
            // The error text names the URL, which holds no secret.
            .with_context(|| format!("reach the Entra token endpoint {url}"))?;
        let status = res.status().as_u16();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        if status != 200 {
            // Only the error code: the description can carry ids and traces.
            let code = body["error"].as_str().unwrap_or("unknown");
            bail!("Entra answered {status} ({code}) to the client credentials sign-in");
        }
        let token = body["access_token"]
            .as_str()
            .context("Entra answered no access token")?
            .to_owned();
        let secs = body["expires_in"].as_u64().unwrap_or(300);
        let until = Instant::now() + Duration::from_secs(secs).saturating_sub(EARLY);
        *cache = Some((token.clone(), until));
        Ok(token)
    }

    async fn forget(&self) {
        *self.cache.lock().await = None;
    }
}

pub(crate) enum Auth {
    /// A personal access token: HTTP basic with an empty user.
    Pat(Secret),
    Entra(Entra),
}

pub(crate) struct Reqwest {
    client: reqwest::Client,
    auth: Auth,
}

impl Reqwest {
    pub(crate) fn new(auth: Auth) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                // A redirect is Azure DevOps sending a bad credential to its
                // sign-in page; following it would post nothing useful.
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .user_agent("kbl-ingest")
                .build()?,
            auth,
        })
    }

    async fn once(&self, req: &Req) -> Result<Res> {
        let method = reqwest::Method::from_bytes(req.method.as_bytes())?;
        let mut b = self
            .client
            .request(method, &req.url)
            .header("accept", "application/json");
        b = match &self.auth {
            Auth::Pat(s) => b.basic_auth("", Some(s.read("Azure DevOps personal access token")?)),
            Auth::Entra(e) => b.bearer_auth(e.token(&self.client).await?),
        };
        if let Some(body) = &req.body {
            b = if req.method == "PATCH" {
                // Work item edits are a JSON Patch document.
                b.header("content-type", "application/json-patch+json")
                    .body(serde_json::to_vec(body)?)
            } else {
                b.json(body)
            };
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

impl Transport for Reqwest {
    async fn send(&self, req: Req) -> Result<Res> {
        let res = self.once(&req).await?;
        let Auth::Entra(e) = &self.auth else {
            return Ok(res);
        };
        // A redirect is the sign-in page, as in `Ado::call`.
        if res.status != 401 && res.status != 203 && !(300..400).contains(&res.status) {
            return Ok(res);
        }
        // The cached token may have been revoked: sign in once more. A second
        // refusal is the credential, not the cache.
        e.forget().await;
        self.once(&req).await
    }
}
