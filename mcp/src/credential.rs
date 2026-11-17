//! Where the agent's Felix token comes from. Docs: README "Giving an agent
//! its tools".
//!
//! A fixed token (`KBL_FELIX_TOKEN`) is used as it is and stops working when it
//! expires. A renewable credential is an ID token that `kbl-mcp` reads again
//! each time it needs a new Felix token and trades at the control plane.
//!
//! The Felix client authenticates a stream once, when it opens it, and keeps
//! its pooled publish and cache streams for the life of the connection. A
//! broker that forwards a write to the shard's owner sends the token that
//! stream opened with, and the owner checks its expiry. So on each renewal
//! `kbl-mcp` opens a new connection and swaps it in ([`reconnect_on_renewal`]).
//! A held claim survives the swap: Felix ties an ack or an extension to the
//! record and its delivery, not to the connection.
//!
//! Every secret comes from the environment or a file. None goes into an error:
//! failures name what went wrong and never echo a token, a response body or the
//! command line.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use felix_client::{TokenFuture, TokenProvider};
use reqwest::Url;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// How long the control plane or the ID-token command may take. The two add up
/// to less than the 30 s the Felix client gives a token fetch.
const STEP_TIMEOUT: Duration = Duration::from_secs(12);
/// The most an ID token or a Felix token may be.
const MAX_TOKEN_BYTES: usize = 16 * 1024;
/// The audience of the Felix token the broker accepts.
const BROKER_AUDIENCE: &str = "felix-broker";
/// How long after a failed renewal the next one waits, so a down control plane
/// is not asked once per stream open.
const RETRY_AFTER: Duration = Duration::from_secs(5);

/// How `kbl-mcp` authenticates to the broker.
pub(crate) enum Auth {
    /// A Felix token used until it expires.
    Fixed(String),
    /// A provider that trades an ID token for a Felix token before the last
    /// one expires.
    Renewing(Arc<Renewer>),
}

/// Where the ID token is read from.
#[derive(Clone)]
enum IdSource {
    /// A file, read again on every renewal so a rotated file is picked up.
    File(PathBuf),
    /// A command whose output is the token, run on every renewal.
    Command(String),
}

struct Exchange {
    url: Url,
    source: IdSource,
    http: reqwest::Client,
}

impl Auth {
    /// Chooses the credential from the environment. `get` reads one variable.
    pub(crate) fn from_env(get: impl Fn(&str) -> Option<String>, tenant: &str) -> Result<Auth> {
        let set = |name: &str| get(name).filter(|v| !v.is_empty());
        let fixed = set("KBL_FELIX_TOKEN");
        let file = set("KBL_FELIX_ID_TOKEN_FILE");
        let command = set("KBL_FELIX_ID_TOKEN_COMMAND");
        let source = match (file, command) {
            (Some(_), Some(_)) => {
                bail!("set only one of KBL_FELIX_ID_TOKEN_FILE and KBL_FELIX_ID_TOKEN_COMMAND")
            }
            (Some(file), None) => Some(IdSource::File(file.into())),
            (None, Some(command)) => Some(IdSource::Command(command)),
            (None, None) => None,
        };
        match (fixed, source) {
            (Some(_), Some(_)) => bail!(
                "set KBL_FELIX_TOKEN or an ID token (KBL_FELIX_ID_TOKEN_FILE or KBL_FELIX_ID_TOKEN_COMMAND), not both"
            ),
            (Some(token), None) => Ok(Auth::Fixed(token)),
            (None, Some(source)) => {
                let base = set("KBL_FELIX_CONTROL_PLANE").context(
                    "set KBL_FELIX_CONTROL_PLANE to the control plane's address to renew from an ID token",
                )?;
                let max_age = set("KBL_FELIX_TOKEN_MAX_AGE_MS")
                    .map(|v| v.parse::<u64>().map(Duration::from_millis))
                    .transpose()
                    .context("KBL_FELIX_TOKEN_MAX_AGE_MS is not a number")?;
                Ok(Auth::Renewing(Arc::new(Renewer {
                    exchange: Exchange::new(&base, tenant, source)?,
                    max_age,
                    held: Mutex::new(None),
                    rejected: std::sync::Mutex::new(None),
                })))
            }
            (None, None) => bail!(
                "set KBL_FELIX_TOKEN to the agent's Felix token, or KBL_FELIX_ID_TOKEN_FILE or KBL_FELIX_ID_TOKEN_COMMAND to renew it from an ID token"
            ),
        }
    }
}

/// The Felix token in hand, replaced before it expires.
pub(crate) struct Renewer {
    exchange: Exchange,
    /// Renew at least this often. For tests; the token's own life decides otherwise.
    max_age: Option<Duration>,
    /// Held across a renewal, so callers that arrive meanwhile wait for its answer.
    held: Mutex<Option<Held>>,
    /// A token the broker refused, until the next `token()` sees it.
    rejected: std::sync::Mutex<Option<String>>,
}

struct Held {
    token: String,
    /// Unix seconds, from the token's `exp`. `None` when it names none.
    expires_at: Option<i64>,
    renew_at: Instant,
}

impl Renewer {
    fn held(&self, token: String) -> Held {
        let now = Instant::now();
        let expires_at = read_exp(&token);
        // Two thirds of the way to expiry leaves the last third for retries.
        let mut renew_at = expires_at
            .map(|exp| now + Duration::from_secs((exp - unix_now()).max(0) as u64) * 2 / 3);
        if let Some(max) = self.max_age {
            renew_at = Some(renew_at.map_or(now + max, |at| at.min(now + max)));
        }
        Held {
            token,
            expires_at,
            // A token that names no expiry is kept until the broker refuses it.
            renew_at: renew_at.unwrap_or(now + Duration::from_secs(365 * 24 * 3600)),
        }
    }
}

impl Renewer {
    /// The Felix token in hand, if one was fetched.
    pub(crate) async fn current(&self) -> Option<String> {
        self.held.lock().await.as_ref().map(|h| h.token.clone())
    }

    /// How long until the token in hand is due for renewal.
    async fn until_due(&self) -> Duration {
        self.held.lock().await.as_ref().map_or(Duration::ZERO, |h| {
            h.renew_at.saturating_duration_since(Instant::now())
        })
    }
}

/// Renews the token when it is due, without waiting for a stream to open, and
/// after each renewal swaps in a connection from `connect` whose streams open
/// with the new token. `connected_with` is the token the first connection used.
/// Runs until the task is dropped.
pub(crate) async fn reconnect_on_renewal<C, F, Fut>(
    renewer: Arc<Renewer>,
    slot: Arc<std::sync::RwLock<Arc<C>>>,
    mut connected_with: Option<String>,
    connect: F,
) where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<C>>,
{
    loop {
        // A renewal that a stream open made counts too.
        if renewer.current().await == connected_with {
            tokio::time::sleep(renewer.until_due().await).await;
            if let Err(e) = renewer.token().await {
                tracing::warn!(error = %format!("{e:#}"), "could not renew the Felix token");
                tokio::time::sleep(RETRY_AFTER).await;
                continue;
            }
            if renewer.current().await == connected_with {
                // The renewal failed and the token in hand is still good.
                continue;
            }
        }
        // Taken before connecting: a renewal during the connect leaves some
        // streams on this token, so the next pass reconnects again.
        let renewed = renewer.current().await;
        match connect().await {
            Ok(client) => {
                *slot.write().expect("client lock") = Arc::new(client);
                connected_with = renewed;
                tracing::debug!("renewed the Felix token and reconnected");
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "could not reconnect with the renewed Felix token");
                tokio::time::sleep(RETRY_AFTER).await;
            }
        }
    }
}

impl TokenProvider for Renewer {
    fn token(&self) -> TokenFuture<'_> {
        Box::pin(async move {
            let mut held = self.held.lock().await;
            let refused = self.rejected.lock().expect("rejected lock").take();
            if let (Some(h), Some(refused)) = (held.as_mut(), refused)
                && h.token == refused
            {
                h.renew_at = Instant::now();
            }
            if let Some(h) = held.as_ref()
                && Instant::now() < h.renew_at
            {
                return Ok(h.token.clone());
            }
            match self.exchange.felix_token().await {
                Ok(token) => {
                    let next = self.held(token.clone());
                    *held = Some(next);
                    Ok(token)
                }
                Err(e) => match held.as_mut() {
                    // The Felix token in hand still works, so a control plane
                    // that is down or a credential that was revoked costs nothing
                    // until it runs out.
                    Some(h) if h.expires_at.is_none_or(|exp| unix_now() < exp) => {
                        h.renew_at = Instant::now() + RETRY_AFTER;
                        tracing::warn!(error = %format!("{e:#}"), "could not renew the Felix token; using the current one");
                        Ok(h.token.clone())
                    }
                    _ => Err(e.context("renew the Felix token")),
                },
            }
        })
    }

    fn invalidate(&self, rejected: &str) {
        // Noted outside the token lock, which a stream open may hold; the next
        // `token()` acts on it.
        *self.rejected.lock().expect("rejected lock") = Some(rejected.to_string());
    }
}

/// A JWT's `exp` claim, unverified: it only decides when to renew, and the
/// broker does the real check.
fn read_exp(token: &str) -> Option<i64> {
    #[derive(serde::Deserialize)]
    struct Claims {
        exp: i64,
    }
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice::<Claims>(&bytes).ok().map(|c| c.exp)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Exchange {
    fn new(base: &str, tenant: &str, source: IdSource) -> Result<Exchange> {
        Ok(Exchange {
            url: exchange_url(base, tenant)?,
            source,
            http: http_client()?,
        })
    }

    /// Reads the ID token and trades it for a Felix token.
    async fn felix_token(&self) -> Result<String> {
        let id_token = read_id_token(&self.source).await?;
        let response = self
            .http
            .post(self.url.clone())
            .bearer_auth(&id_token)
            .json(&json!({ "audience": BROKER_AUDIENCE }))
            .send()
            .await
            .map_err(|e| anyhow!("reach the control plane: {}", e.without_url()))?;
        let status = response.status();
        if !status.is_success() {
            // The body is left out: an error page may echo the request.
            bail!(
                "the control plane refused the ID token (status {}); it may be expired or revoked",
                status.as_u16()
            );
        }
        let body: Value = response
            .json()
            .await
            .map_err(|e| anyhow!("read the control plane's answer: {}", e.without_url()))?;
        let token = body
            .get("felix_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty() && t.len() <= MAX_TOKEN_BYTES)
            .context("the control plane's answer has no felix_token")?;
        Ok(token.to_string())
    }
}

fn exchange_url(base: &str, tenant: &str) -> Result<Url> {
    let mut url = Url::parse(base).context("KBL_FELIX_CONTROL_PLANE is not a URL")?;
    // The ID token is a bearer credential: never send it in the clear
    // beyond this machine.
    let local = match url.host() {
        Some(url::Host::Domain(name)) => name == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    match url.scheme() {
        "https" => {}
        "http" if local => {}
        "http" => bail!("KBL_FELIX_CONTROL_PLANE must be https unless it is on this machine"),
        _ => bail!("KBL_FELIX_CONTROL_PLANE must be an http or https URL"),
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("KBL_FELIX_CONTROL_PLANE must not carry a user name or password");
    }
    url.path_segments_mut()
        .map_err(|()| anyhow!("KBL_FELIX_CONTROL_PLANE cannot be a base URL"))?
        .pop_if_empty()
        .extend(["v1", "tenants", tenant, "token", "exchange"]);
    Ok(url)
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(STEP_TIMEOUT)
        // A redirect would carry the ID token to wherever it points.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("set up the control plane client")
}

async fn read_id_token(source: &IdSource) -> Result<String> {
    let raw = match source {
        IdSource::File(path) => read_file(path).await?,
        IdSource::Command(command) => run_command(command).await?,
    };
    let token = raw.trim();
    if token.is_empty() {
        bail!("the ID token source gave an empty token");
    }
    if token.len() > MAX_TOKEN_BYTES || token.chars().any(char::is_whitespace) {
        bail!("the ID token source gave something that is not a token");
    }
    Ok(token.to_string())
}

async fn read_file(path: &Path) -> Result<String> {
    let shown = path.display();
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("read the ID token file {shown}"))?;
    // The opened file's mode, so a swap after the check cannot change what is read.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = file
            .metadata()
            .await
            .with_context(|| format!("read the ID token file {shown}"))?;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!("the ID token file {shown} must not be readable by others; run chmod 600 on it");
        }
    }
    let mut bytes = Vec::new();
    // One byte over the limit is enough to tell it is too big.
    (&mut file)
        .take(MAX_TOKEN_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("read the ID token file {shown}"))?;
    String::from_utf8(bytes).map_err(|_| anyhow!("the ID token file {shown} is not text"))
}

async fn run_command(command: &str) -> Result<String> {
    #[cfg(unix)]
    let mut cmd = {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    };
    #[cfg(not(unix))]
    let mut cmd = {
        let mut cmd = tokio::process::Command::new("cmd");
        cmd.arg("/C").arg(command);
        cmd
    };
    // Its stderr is dropped: a tool that fails may print the token it read.
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("start the ID token command")?;
    let mut stdout = child.stdout.take().context("start the ID token command")?;
    let run = async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(MAX_TOKEN_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((bytes, status))
    };
    let (bytes, status) = tokio::time::timeout(STEP_TIMEOUT, run)
        .await
        .map_err(|_| {
            anyhow!(
                "the ID token command took longer than {}s",
                STEP_TIMEOUT.as_secs()
            )
        })?
        .context("run the ID token command")?;
    if !status.success() {
        bail!("the ID token command failed ({status})");
    }
    String::from_utf8(bytes).map_err(|_| anyhow!("the ID token command did not print text"))
}

#[cfg(test)]
mod tests;
