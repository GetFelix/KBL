//! The worker's side of source leases: the config long poll and the lease
//! heartbeat, each on its own task, and the local upstream fence every vendor
//! call passes. Design: docs/design/sources.md#state-leases-and-workers.
//!
//! A worker whose `tracker.group` is unset, or whose group no connection
//! names, is file-managed: it polls every configured repo and nothing here
//! stops it.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use kbl_core::lease::{FENCE_MS, HEARTBEAT_MS};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::source::{
    Changes, Comment, Cursor, Fetched, IssueRef, Links, PullRequest, Releases, Source, SourceError,
    WriteBack,
};

const FENCE: Duration = Duration::from_millis(FENCE_MS);
const HEARTBEAT: Duration = Duration::from_millis(HEARTBEAT_MS);
/// The longest a config poll is held.
const CONFIG_WAIT_S: u64 = 60;
/// The wait after a failed config poll.
const CONFIG_RETRY: Duration = Duration::from_secs(5);

/// What the worker may poll, as the config and heartbeat answers left it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    /// The config has not answered yet: no vendor call.
    Waiting,
    /// No connection: every configured repo, unfenced.
    FileManaged,
    Leased {
        /// The connection's sources, ids like `github:o/r`.
        sources: BTreeSet<String>,
        /// Source to the epoch held at the last successful renew.
        held: BTreeMap<String, u64>,
        /// When that renew was sent, by the monotonic clock. Taken before
        /// the request, so the tracker's 90 s starts after it.
        renewed: Option<Instant>,
    },
}

/// The worker's leases and fence, shared by the poll, the config task and
/// the heartbeat task.
pub(crate) struct Lessee {
    /// The vendor word sources start with: `github` in `github:o/r`.
    vendor: &'static str,
    mode: Mutex<Mode>,
    /// Wakes the poll loop and the heartbeat when the mode changes.
    pub(crate) changed: Notify,
}

impl Lessee {
    /// A worker with no group: file-managed for good.
    pub(crate) fn file_managed(vendor: &'static str) -> Arc<Self> {
        Self::new(vendor, Mode::FileManaged)
    }

    /// A worker with a group, which polls nothing until the config answers.
    pub(crate) fn waiting(vendor: &'static str) -> Arc<Self> {
        Self::new(vendor, Mode::Waiting)
    }

    fn new(vendor: &'static str, mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            vendor,
            mode: Mutex::new(mode),
            changed: Notify::new(),
        })
    }

    fn source_of(&self, repo: &str) -> String {
        format!("{}:{repo}", self.vendor)
    }

    /// Takes the config answer's `connection`: null means file-managed.
    pub(crate) fn configure(&self, connection: &Value) {
        let mut mode = self.mode.lock().expect("lessee lock");
        let next = match connection["sources"].as_array() {
            None => Mode::FileManaged,
            Some(list) => {
                let sources: BTreeSet<String> = list
                    .iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect();
                match &*mode {
                    Mode::Leased { held, renewed, .. } => Mode::Leased {
                        // A source the connection dropped is no longer held.
                        held: held
                            .iter()
                            .filter(|(s, _)| sources.contains(*s))
                            .map(|(s, e)| (s.clone(), *e))
                            .collect(),
                        renewed: *renewed,
                        sources,
                    },
                    _ => Mode::Leased {
                        sources,
                        held: BTreeMap::new(),
                        renewed: None,
                    },
                }
            }
        };
        if *mode != next {
            *mode = next;
            drop(mode);
            self.changed.notify_waiters();
        }
    }

    pub(crate) fn leased(&self) -> bool {
        matches!(*self.mode.lock().expect("lessee lock"), Mode::Leased { .. })
    }

    /// A successful heartbeat sent at `sent` answered `leases`. Anything not in
    /// it is no longer held, so the fence stops it at once.
    pub(crate) fn renewed(&self, sent: Instant, leases: BTreeMap<String, u64>) {
        let mut mode = self.mode.lock().expect("lessee lock");
        if let Mode::Leased {
            sources,
            held,
            renewed,
        } = &mut *mode
        {
            *held = leases
                .into_iter()
                .filter(|(s, _)| sources.contains(s))
                .collect();
            *renewed = Some(sent);
        }
        drop(mode);
        self.changed.notify_waiters();
    }

    /// The epoch the worker holds `source` at, while its fence is open. The
    /// tracker will fence state and record writes with it.
    #[allow(dead_code)]
    pub(crate) fn epoch(&self, source: &str) -> Option<u64> {
        match &*self.mode.lock().expect("lessee lock") {
            Mode::Leased { held, renewed, .. } if fresh(*renewed) => held.get(source).copied(),
            _ => None,
        }
    }

    /// Whether a poll may run now: file-managed, or holding a lease inside
    /// the fence. A poll that may not run makes no tracker or vendor call.
    pub(crate) fn may_poll(&self) -> bool {
        match &*self.mode.lock().expect("lessee lock") {
            Mode::Waiting => false,
            Mode::FileManaged => true,
            Mode::Leased { held, renewed, .. } => !held.is_empty() && fresh(*renewed),
        }
    }

    /// The fence, checked right before every vendor call. With leases, a call
    /// is refused once the last successful renew is older than 60 s, and a
    /// call into one of the connection's sources also needs its lease. A
    /// repo outside the connection (another source's pull request) needs only
    /// the first.
    pub(crate) fn check(&self, repo: Option<&str>) -> Result<(), SourceError> {
        match &*self.mode.lock().expect("lessee lock") {
            Mode::FileManaged => Ok(()),
            Mode::Waiting => Err(fenced("the config has not answered yet")),
            Mode::Leased { renewed, .. } if !fresh(*renewed) => Err(fenced(
                "no lease renew in the last 60 s; no upstream call until one succeeds",
            )),
            Mode::Leased { sources, held, .. } => match repo.map(|r| self.source_of(r)) {
                Some(s) if sources.contains(&s) && !held.contains_key(&s) => {
                    Err(fenced("another worker holds this source's lease"))
                }
                _ => Ok(()),
            },
        }
    }

    /// `repos` cut to the ones this worker may poll now.
    fn pollable(&self, repos: &[String]) -> Vec<String> {
        repos
            .iter()
            .filter(|r| self.check(Some(r)).is_ok())
            .cloned()
            .collect()
    }
}

fn fresh(renewed: Option<Instant>) -> bool {
    renewed.is_some_and(|at| at.elapsed() <= FENCE)
}

fn fenced(why: &str) -> SourceError {
    SourceError::Failed(anyhow::anyhow!("lease fence: {why}"))
}

/// The tracker calls the two lease tasks make.
pub(crate) trait LeaseApi: Send + Sync {
    /// `PUT /v1/ingest/leases`: the leases held after it, by source.
    fn heartbeat(
        &self,
        group: &str,
        instance: &str,
        leave: bool,
    ) -> impl Future<Output = Result<BTreeMap<String, u64>>> + Send;

    /// `GET /v1/ingest/config`: the answer and its `ETag`, or none when it did
    /// not change. `Ok(Some((Value::Null, _)))` means the tracker has no
    /// config route or no teams, so the worker is file-managed.
    fn config(
        &self,
        group: &str,
        instance: &str,
        etag: Option<&str>,
        wait_s: u64,
    ) -> impl Future<Output = Result<Option<(Value, String)>>> + Send;
}

/// One heartbeat. On failure the leases stand as they were and the fence
/// closes on its own once the last success is 60 s old.
pub(crate) async fn heartbeat_once<A: LeaseApi>(
    api: &A,
    lessee: &Lessee,
    group: &str,
    instance: &str,
) {
    if !lessee.leased() {
        return;
    }
    let sent = Instant::now();
    match api.heartbeat(group, instance, false).await {
        Ok(leases) => lessee.renewed(sent, leases),
        Err(e) => eprintln!("kbl-ingest: lease heartbeat failed: {e:#}"),
    }
}

/// Renews every 30 s, apart from polling, and at once when the config moves
/// the worker to leases.
pub(crate) async fn heartbeat_task<A: LeaseApi>(
    api: Arc<A>,
    lessee: Arc<Lessee>,
    group: String,
    instance: String,
) {
    loop {
        let became = lessee.changed.notified();
        let was = lessee.leased();
        heartbeat_once(&*api, &lessee, &group, &instance).await;
        tokio::select! {
            () = tokio::time::sleep(HEARTBEAT) => {}
            () = became, if !was => {}
        }
    }
}

/// Holds the config poll and applies each change.
pub(crate) async fn config_task<A: LeaseApi>(
    api: Arc<A>,
    lessee: Arc<Lessee>,
    group: String,
    instance: String,
) {
    let mut etag: Option<String> = None;
    loop {
        match api
            .config(&group, &instance, etag.as_deref(), CONFIG_WAIT_S)
            .await
        {
            Ok(Some((body, tag))) => {
                lessee.configure(&body["connection"]);
                etag = Some(tag);
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("kbl-ingest: config poll failed: {e:#}");
                tokio::time::sleep(CONFIG_RETRY).await;
            }
        }
    }
}

/// A new instance name, so two workers on one token are told apart.
pub(crate) fn instance() -> Result<String> {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 8];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("drawing an instance name"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl LeaseApi for crate::tracker_api::Http {
    async fn heartbeat(
        &self,
        group: &str,
        instance: &str,
        leave: bool,
    ) -> Result<BTreeMap<String, u64>> {
        let body = json!({ "group": group, "instance": instance, "leave": leave });
        let (status, body) = self.put_json("/v1/ingest/leases", &body).await?;
        if status != 200 {
            bail!("the tracker answered {status} to the lease heartbeat: {body}");
        }
        Ok(body["leases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| Some((l["source"].as_str()?.to_owned(), l["epoch"].as_u64()?)))
            .collect())
    }

    async fn config(
        &self,
        group: &str,
        instance: &str,
        etag: Option<&str>,
        wait_s: u64,
    ) -> Result<Option<(Value, String)>> {
        let (status, body, tag) = self.get_config(group, instance, etag, wait_s).await?;
        match status {
            200 => Ok(Some((body, tag.unwrap_or_default()))),
            304 => Ok(None),
            // No config route, or a tracker without teams: file-managed.
            404 => Ok(Some((Value::Null, String::new()))),
            _ => bail!("the tracker answered {status} to the config poll: {body}"),
        }
    }
}

/// A source whose every vendor call passes the fence first.
pub(crate) struct Fenced<S> {
    inner: S,
    lessee: Arc<Lessee>,
}

impl<S> Fenced<S> {
    pub(crate) fn new(inner: S, lessee: Arc<Lessee>) -> Self {
        Self { inner, lessee }
    }
}

fn written_repo(what: &WriteBack) -> &str {
    match what {
        WriteBack::DraftPullRequest { pr, .. } => &pr.repo,
        WriteBack::Comment { issue, .. }
        | WriteBack::LinkPullRequest { issue, .. }
        | WriteBack::Transition { issue, .. } => &issue.repo,
    }
}

impl<S: Source> Source for Fenced<S> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn reads_pull_requests(&self) -> bool {
        self.inner.reads_pull_requests()
    }

    fn own_logins(&self) -> Vec<String> {
        self.inner.own_logins()
    }

    async fn changed(
        &self,
        repos: &[String],
        cursors: &BTreeMap<String, Cursor>,
    ) -> Result<Changes, SourceError> {
        self.lessee.check(None)?;
        let repos = self.lessee.pollable(repos);
        if repos.is_empty() {
            return Ok(Changes::default());
        }
        self.inner.changed(&repos, cursors).await
    }

    async fn pull_request(&self, id: &IssueRef) -> Result<Option<PullRequest>, SourceError> {
        self.lessee.check(Some(&id.repo))?;
        self.inner.pull_request(id).await
    }

    async fn fetch(&self, id: &IssueRef) -> Result<Fetched, SourceError> {
        self.lessee.check(Some(&id.repo))?;
        self.inner.fetch(id).await
    }

    async fn issue_comments(&self, id: &IssueRef) -> Result<Vec<Comment>, SourceError> {
        self.lessee.check(Some(&id.repo))?;
        self.inner.issue_comments(id).await
    }

    async fn links(&self, id: &IssueRef) -> Result<Links, SourceError> {
        self.lessee.check(Some(&id.repo))?;
        self.inner.links(id).await
    }

    async fn write_back(&self, what: &WriteBack) -> Result<(), SourceError> {
        // Checked last thing before the write, as a write-back is the one
        // upstream call a second worker could repeat.
        self.lessee.check(Some(written_repo(what)))?;
        self.inner.write_back(what).await
    }

    async fn releases(&self, repo: &str, since_ms: u64) -> Result<Releases, SourceError> {
        self.lessee.check(Some(repo))?;
        self.inner.releases(repo, since_ms).await
    }

    async fn contains(&self, repo: &str, tag: &str, sha: &str) -> Result<bool, SourceError> {
        self.lessee.check(Some(repo))?;
        self.inner.contains(repo, tag, sha).await
    }

    async fn open_issue(
        &self,
        repo: &str,
        title: &str,
        body: &str,
    ) -> Result<IssueRef, SourceError> {
        self.lessee.check(Some(repo))?;
        self.inner.open_issue(repo, title, body).await
    }
}

#[cfg(test)]
mod tests;
