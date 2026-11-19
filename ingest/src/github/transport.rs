//! The HTTP under the GitHub adapter, as a trait so tests run it against
//! recorded answers and never reach GitHub.

use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::app::AppAuth;

#[derive(Debug, Clone)]
pub(crate) struct Req {
    pub(crate) method: &'static str,
    pub(crate) url: String,
    pub(crate) if_none_match: Option<String>,
    pub(crate) body: Option<Value>,
    /// Read the answer as a log: its last this-many bytes, as text.
    pub(crate) tail: Option<usize>,
    /// Send no token: a signed log link GitHub redirects to refuses one.
    pub(crate) anon: bool,
}

impl Req {
    pub(crate) fn get(url: String) -> Self {
        Self {
            method: "GET",
            url,
            if_none_match: None,
            body: None,
            tail: None,
            anon: false,
        }
    }

    /// A GET for a log, kept to its last `max` bytes. `anon` sends no token.
    pub(crate) fn log(url: String, max: usize, anon: bool) -> Self {
        Self {
            tail: Some(max),
            anon,
            ..Self::get(url)
        }
    }

    pub(crate) fn post(url: String, body: Value) -> Self {
        Self {
            method: "POST",
            url,
            if_none_match: None,
            body: Some(body),
            tail: None,
            anon: false,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Res {
    pub(crate) status: u16,
    /// Header names in lower case.
    pub(crate) headers: BTreeMap<String, String>,
    /// Null when there is no JSON body; a string for a log read.
    pub(crate) body: Value,
}

pub(crate) trait Transport: Send + Sync {
    fn send(&self, req: Req) -> impl Future<Output = Result<Res>> + Send;
}

/// Where the token comes from.
pub(crate) enum Token {
    File(PathBuf),
    Env(String),
    /// Installation tokens ingest mints and refreshes itself.
    App(Box<AppAuth>),
}

impl Token {
    /// Read for every request, so a refreshed App installation token is picked up.
    async fn read(&self) -> Result<String> {
        let token = match self {
            Token::App(app) => return app.token().await,
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
                .user_agent("kbl-ingest")
                .build()?,
            token,
        })
    }
}

impl Transport for Reqwest {
    async fn send(&self, req: Req) -> Result<Res> {
        if req.anon {
            return self.send_with(&req, None).await;
        }
        let token = self.token.read().await?;
        let res = self.send_with(&req, Some(&token)).await?;
        let Token::App(app) = &self.token else {
            return Ok(res);
        };
        if res.status != 401 {
            return Ok(res);
        }
        // The cached token may have been revoked or may have outlived its
        // clock: mint once more. A second 401 is the credential, not the cache.
        app.forget(&token).await;
        let token = app.token().await?;
        let res = self.send_with(&req, Some(&token)).await?;
        if res.status == 401 {
            bail!(
                "GitHub answered 401 to a freshly minted installation token for App {}; \
                 check the App id, the private key and that the installation still exists",
                app.id()
            );
        }
        Ok(res)
    }
}

impl Reqwest {
    async fn send_with(&self, req: &Req, token: Option<&str>) -> Result<Res> {
        let method = reqwest::Method::from_bytes(req.method.as_bytes())?;
        let mut b = self.client.request(method, &req.url);
        if let Some(token) = token {
            b = b
                .bearer_auth(token)
                .header("accept", "application/vnd.github+json")
                .header("x-github-api-version", "2022-11-28");
        }
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
        let headers: BTreeMap<String, String> = res
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned())))
            .collect();
        let body = match req.tail {
            Some(max) if status == 200 => Value::String(read_tail(res, max).await?),
            Some(_) => Value::Null,
            None => res.json().await.unwrap_or(Value::Null),
        };
        Ok(Res {
            status,
            headers,
            body,
        })
    }
}

/// Most bytes of one log ingest reads. A log can be far larger than the
/// excerpt it keeps; past this it stops and says the excerpt is not the end.
const MAX_LOG_READ: usize = 32 << 20;

/// Streams a log body into its last `max` bytes. The body is bytes whatever
/// its content type says, and is decoded lossily.
async fn read_tail(mut res: reqwest::Response, max: usize) -> Result<String> {
    let mut tail = Tail::new(max, MAX_LOG_READ);
    while let Some(chunk) = res.chunk().await.context("read a log")? {
        if !tail.push(&chunk) {
            break;
        }
    }
    Ok(tail.finish())
}

/// The last `keep` bytes of a stream that is read for at most `cap` bytes.
pub(crate) struct Tail {
    keep: usize,
    cap: usize,
    read: usize,
    ring: VecDeque<u8>,
    cut: bool,
}

impl Tail {
    pub(crate) fn new(keep: usize, cap: usize) -> Self {
        Self {
            keep,
            cap,
            read: 0,
            ring: VecDeque::with_capacity(keep),
            cut: false,
        }
    }

    /// Takes the next bytes; false once the cap is passed and reading should stop.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> bool {
        let room = self.cap - self.read;
        let take = &bytes[..bytes.len().min(room)];
        self.read += take.len();
        let take = &take[take.len().saturating_sub(self.keep)..];
        let over = (self.ring.len() + take.len()).saturating_sub(self.keep);
        self.ring.drain(..over);
        self.ring.extend(take);
        if bytes.len() > room {
            self.cut = true;
        }
        !self.cut
    }

    /// The excerpt as text of at most `keep` bytes, noting a cut read.
    pub(crate) fn finish(self) -> String {
        let bytes: Vec<u8> = self.ring.into_iter().collect();
        // The ring may start inside a character.
        let start = bytes
            .iter()
            .take(3)
            .take_while(|b| (**b & 0b1100_0000) == 0b1000_0000)
            .count();
        let text = String::from_utf8_lossy(&bytes[start..]);
        let note = if self.cut {
            format!("[log cut after {} bytes; this is not its end]\n", self.cap)
        } else {
            String::new()
        };
        let room = self.keep.saturating_sub(note.len());
        format!("{note}{}", crate::sync::tail(&text, room))
    }
}

#[cfg(test)]
mod tests;
