//! GitHub App credentials: sign a short-lived JWT with the App's private
//! key, trade it for an installation token, and keep that token until it is
//! about to expire. The key, the JWT and the token are never logged and
//! never put in an error.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::budget::Budget;
use crate::config::{AppConfig, Installation};
use crate::github::Res;

/// The JWT is backdated this far, so a clock a little ahead of GitHub's
/// does not make it "issued in the future".
const IAT_SKEW: u64 = 60;
/// GitHub refuses a JWT valid for more than ten minutes. The expiry is the
/// whole of that window counted from `iat`, so it stays under the limit.
const JWT_LIFETIME: u64 = 9 * 60;
/// A cached token is replaced once this little of its life is left.
const REFRESH_BEFORE: u64 = 5 * 60;
/// A token minted this recently is reused whatever its expiry says, so a
/// local clock far ahead of GitHub's does not mint on every request.
const REUSE_FOR: u64 = 60;

/// Unix seconds. A function so tests move time.
pub(crate) type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;
/// Where progress lines go. A function so tests can read them.
pub(crate) type Log = Arc<dyn Fn(&str) + Send + Sync>;

pub(crate) fn system_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    })
}

pub(crate) fn stderr_log() -> Log {
    Arc::new(|m| eprintln!("kbl-ingest: {m}"))
}

struct Cached {
    token: String,
    expires_at: u64,
    /// Local clock reading at the mint.
    minted_at: u64,
}

pub(crate) struct AppAuth {
    app_id: String,
    installation: Installation,
    key: RsaKeyPair,
    api: String,
    client: reqwest::Client,
    clock: Clock,
    log: Log,
    /// Charged for the mint and the installation lookup, which are requests
    /// to the vendor like any other.
    budget: Option<Arc<Budget>>,
    // Held across the mint, so requests that arrive together mint once.
    cache: Mutex<Option<Cached>>,
    installation_id: Mutex<Option<u64>>,
}

impl fmt::Debug for AppAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppAuth")
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}

impl AppAuth {
    pub(crate) fn new(cfg: &AppConfig, api: &str, clock: Clock, log: Log) -> Result<Self> {
        // The key signs every mint, so like the tracker's token file it must
        // be private to its owner; a looser file refuses the start.
        check_key_mode(&cfg.private_key_file)?;
        if !api.starts_with("https://") {
            log(&format!(
                "warning: the GitHub api base {api} is not https; the App JWT and installation tokens would cross the network in the clear"
            ));
        }
        let pem = std::fs::read_to_string(&cfg.private_key_file).with_context(|| {
            format!(
                "read the GitHub App private key {}",
                cfg.private_key_file.display()
            )
        })?;
        let key = parse_key(&pem).with_context(|| {
            format!(
                "load the GitHub App private key {}",
                cfg.private_key_file.display()
            )
        })?;
        Ok(Self {
            app_id: cfg.id.clone(),
            installation: cfg.installation.clone(),
            key,
            api: api.trim_end_matches('/').to_owned(),
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .user_agent("kbl-ingest")
                .build()?,
            clock,
            log,
            budget: None,
            cache: Mutex::new(None),
            installation_id: Mutex::new(match cfg.installation {
                Installation::Id(id) => Some(id),
                _ => None,
            }),
        })
    }

    pub(crate) fn with_budget(mut self, budget: Arc<Budget>) -> Self {
        self.budget = Some(budget);
        self
    }

    /// The App id, for error messages. Not a secret.
    pub(crate) fn id(&self) -> &str {
        &self.app_id
    }

    /// A token with more than the refresh margin left, minted if needed.
    pub(crate) async fn token(&self) -> Result<String> {
        let mut cache = self.cache.lock().await;
        let now = (self.clock)();
        if let Some(c) = cache.as_ref()
            && (c.expires_at > now + REFRESH_BEFORE
                || (c.minted_at..c.minted_at + REUSE_FOR).contains(&now))
        {
            return Ok(c.token.clone());
        }
        let fresh = self.mint(now).await?;
        let token = fresh.token.clone();
        *cache = Some(fresh);
        Ok(token)
    }

    /// Drops the cached token, for GitHub having refused it.
    pub(crate) async fn forget(&self, token: &str) {
        let mut cache = self.cache.lock().await;
        // Another request may have refreshed already; keep that one.
        if cache.as_ref().is_some_and(|c| c.token == token) {
            *cache = None;
        }
    }

    async fn mint(&self, now: u64) -> Result<Cached> {
        let jwt = self.jwt(now)?;
        // A looked-up installation id goes stale when the App is uninstalled
        // and installed again, so a refused id is dropped and found once more.
        let mut rediscovered = false;
        let (id, status, body) = loop {
            let id = self.installation_id(&jwt).await?;
            let url = format!("{}/app/installations/{id}/access_tokens", self.api);
            let (status, body) = self.call("POST", &url, &jwt).await?;
            let stale = status == 404 || status == 401;
            if stale && !rediscovered && !matches!(self.installation, Installation::Id(_)) {
                rediscovered = true;
                *self.installation_id.lock().await = None;
                (self.log)(&format!(
                    "GitHub answered {status} for installation {id}; looking the installation up again"
                ));
                continue;
            }
            break (id, status, body);
        };
        if status != 201 && status != 200 {
            bail!(
                "GitHub refused to mint an installation token for App {} installation {id} ({status}): {}",
                self.app_id,
                message(&body)
            );
        }
        let token = body["token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| anyhow!("GitHub's installation token answer has no token"))?
            .to_owned();
        let expires_at = body["expires_at"]
            .as_str()
            .and_then(super::parse_utc_ms)
            .map(|ms| ms / 1000)
            .ok_or_else(|| anyhow!("GitHub's installation token answer has no valid expires_at"))?;
        (self.log)(&format!(
            "minted a GitHub installation token for installation {id}, {}s of life",
            expires_at.saturating_sub(now)
        ));
        Ok(Cached {
            token,
            expires_at,
            minted_at: now,
        })
    }

    async fn installation_id(&self, jwt: &str) -> Result<u64> {
        let mut known = self.installation_id.lock().await;
        if let Some(id) = *known {
            return Ok(id);
        }
        let path = match &self.installation {
            Installation::Repo(r) => format!("repos/{r}/installation"),
            Installation::Org(o) => format!("orgs/{o}/installation"),
            Installation::Id(_) => unreachable!("an id is known from the start"),
        };
        let (status, body) = self
            .call("GET", &format!("{}/{path}", self.api), jwt)
            .await?;
        if status != 200 {
            bail!(
                "GitHub would not say which installation serves {path} ({status}): {}",
                message(&body)
            );
        }
        let id = body["id"]
            .as_u64()
            .ok_or_else(|| anyhow!("GitHub's installation answer has no id"))?;
        *known = Some(id);
        Ok(id)
    }

    async fn call(&self, method: &str, url: &str, jwt: &str) -> Result<(u16, Value)> {
        let res = match &self.budget {
            Some(b) => b.metered(self.send(method, url, jwt)).await?,
            None => self.send(method, url, jwt).await?,
        };
        Ok((res.status, res.body))
    }

    async fn send(&self, method: &str, url: &str, jwt: &str) -> Result<Res> {
        let res = self
            .client
            .request(reqwest::Method::from_bytes(method.as_bytes())?, url)
            .bearer_auth(jwt)
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .send()
            .await
            // A reqwest error can carry the URL but never a header.
            .with_context(|| format!("reach {url}"))?;
        let status = res.status().as_u16();
        let headers = res
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned())))
            .collect();
        Ok(Res {
            status,
            headers,
            body: res.json().await.unwrap_or(Value::Null),
        })
    }

    fn jwt(&self, now: u64) -> Result<String> {
        let iat = now.saturating_sub(IAT_SKEW);
        let claims = json!({ "iat": iat, "exp": iat + JWT_LIFETIME, "iss": self.app_id });
        let head = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let mut signing = format!("{head}.{}", URL_SAFE_NO_PAD.encode(claims.to_string()));
        let mut sig = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signing.as_bytes(),
                &mut sig,
            )
            .map_err(|_| anyhow!("sign the GitHub App JWT"))?;
        signing.push('.');
        signing.push_str(&URL_SAFE_NO_PAD.encode(sig));
        Ok(signing)
    }
}

pub(crate) fn check_key_mode(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .with_context(|| format!("read the GitHub App private key {}", path.display()))?
            .permissions()
            .mode()
            & 0o777;
        anyhow::ensure!(
            mode == 0o600 || mode == 0o400,
            "the GitHub App private key {} has mode {mode:o}; set it to 600 or 400",
            path.display()
        );
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// GitHub's `message`, with any known secret shape cut out.
fn message(body: &Value) -> String {
    let m = body["message"].as_str().unwrap_or("no message");
    kbl_core::redact::redact(m).into_owned()
}

/// An RSA key in PEM, as GitHub hands it out (PKCS#1) or as PKCS#8.
fn parse_key(pem: &str) -> Result<RsaKeyPair> {
    let mut label = None;
    let mut b64 = String::new();
    for line in pem.lines().map(str::trim) {
        if let Some(l) = line
            .strip_prefix("-----BEGIN ")
            .and_then(|l| l.strip_suffix("-----"))
        {
            label = Some(l.to_owned());
        } else if line.starts_with("-----") || line.contains(':') {
            continue;
        } else if label.is_some() {
            b64.push_str(line);
        }
    }
    // The decoder's own error would not echo the input, but say less anyway.
    let der = STANDARD
        .decode(b64)
        .map_err(|_| anyhow!("the key is not valid PEM"))?;
    let pair = match label.as_deref() {
        Some("RSA PRIVATE KEY") => RsaKeyPair::from_der(&der),
        Some("PRIVATE KEY") => RsaKeyPair::from_pkcs8(&der),
        _ => bail!("the key must be a PEM `RSA PRIVATE KEY` or `PRIVATE KEY`"),
    };
    pair.map_err(|_| anyhow!("the key is not a usable RSA private key (2048 bits or more)"))
}

#[cfg(test)]
mod tests;
