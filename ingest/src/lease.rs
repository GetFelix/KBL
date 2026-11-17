//! The worker's side of source leases: the config long poll and the lease
//! heartbeat, each on its own task, and the local upstream fence every vendor
//! call passes. Design: docs/design/sources.md#state-leases-and-workers.
//!
//! A worker whose `tracker.group` is unset, or whose group no connection
//! names, is file-managed: it polls every configured repo and nothing here
//! stops it. A leased worker touches only repos its connection names and it
//! holds, except to read a pull request elsewhere.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use kbl_core::lease::{FENCE_MS, HEARTBEAT_MS};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::github::{Req, Res, Transport};
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
/// How often a vendor request in flight rechecks the fence. The wall clock
/// is read each time, so a request across a suspend is cut within this of
/// waking.
const RECHECK: Duration = Duration::from_secs(1);

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
        /// Source to what the last successful renew answered for it.
        held: BTreeMap<String, Held>,
        /// When that renew was sent. Taken before the request, so the
        /// tracker's 90 s starts after it.
        renewed: Option<Renewed>,
    },
}

/// One source's lease as a heartbeat answered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Held {
    pub(crate) epoch: u64,
    /// For a source whose share moved to another worker: how long its lease
    /// has left, from when the renew was sent. It is not renewed again.
    pub(crate) until: Option<Duration>,
}

/// When a renew was sent, by both clocks. The monotonic clock is immune to
/// a stepped wall clock but stops while the machine sleeps; the wall clock
/// keeps running, so the fence closes on whichever has passed further.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Renewed {
    mono: Instant,
    wall_ms: u64,
}

/// The wall clock in milliseconds, injectable so a test can suspend.
type Wall = Box<dyn Fn() -> u64 + Send + Sync>;

fn system_wall() -> Wall {
    Box::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    })
}

/// The worker's leases and fence, shared by the poll, the config task, the
/// heartbeat task and every vendor request.
pub(crate) struct Lessee {
    /// The vendor word sources start with: `github` in `github:o/r`.
    vendor: &'static str,
    /// The repos the worker's file configures, for the warning about ones
    /// the connection does not name.
    repos: Vec<String>,
    mode: Mutex<Mode>,
    wall: Wall,
    /// Wakes the poll loop, the heartbeat and requests in flight when the
    /// mode changes.
    pub(crate) changed: Notify,
}

impl Lessee {
    /// A worker with no group: file-managed for good.
    pub(crate) fn file_managed(vendor: &'static str) -> Arc<Self> {
        Self::new(vendor, Mode::FileManaged, system_wall())
    }

    /// A worker with a group, which polls nothing until the config answers.
    pub(crate) fn waiting(vendor: &'static str, repos: Vec<String>) -> Arc<Self> {
        let mut l = Self::new(vendor, Mode::Waiting, system_wall());
        Arc::get_mut(&mut l).expect("new").repos = repos;
        l
    }

    fn new(vendor: &'static str, mode: Mode, wall: Wall) -> Arc<Self> {
        Arc::new(Self {
            vendor,
            repos: Vec::new(),
            mode: Mutex::new(mode),
            wall,
            changed: Notify::new(),
        })
    }

    /// The connection source naming `repo`, matched as the vendor compares
    /// names: GitHub, GitLab and Azure DevOps ignore case, Jira keys do not.
    fn source_for<'a>(&self, sources: &'a BTreeSet<String>, repo: &str) -> Option<&'a String> {
        let want = format!("{}:{repo}", self.vendor);
        let same = |s: &&String| match self.vendor {
            "jira" => **s == want,
            _ => s.eq_ignore_ascii_case(&want),
        };
        sources.iter().find(same)
    }

    /// The configured repos the connection does not name. They are never
    /// polled while the worker is leased.
    pub(crate) fn outside(&self) -> Vec<String> {
        match &*self.mode.lock().expect("lessee lock") {
            Mode::Leased { sources, .. } => self
                .repos
                .iter()
                .filter(|r| self.source_for(sources, r).is_none())
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
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
                            .map(|(s, h)| (s.clone(), *h))
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
            let outside = self.outside();
            if !outside.is_empty() {
                eprintln!(
                    "kbl-ingest: warning: the connection does not name {}; they are not polled",
                    outside.join(", ")
                );
            }
            self.changed.notify_waiters();
        }
    }

    pub(crate) fn leased(&self) -> bool {
        matches!(*self.mode.lock().expect("lessee lock"), Mode::Leased { .. })
    }

    /// Both clocks now, taken right before a renew is sent.
    pub(crate) fn sending(&self) -> (Instant, u64) {
        (Instant::now(), (self.wall)())
    }

    /// The sources the worker holds now, which the heartbeat reports.
    pub(crate) fn held(&self) -> Vec<String> {
        match &*self.mode.lock().expect("lessee lock") {
            Mode::Leased { held, .. } => held.keys().cloned().collect(),
            _ => Vec::new(),
        }
    }

    /// A successful heartbeat sent at `sent` answered `leases`. Anything not in
    /// it is no longer held, so the fence stops it at once.
    pub(crate) fn renewed(&self, sent: (Instant, u64), leases: BTreeMap<String, Held>) {
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
            *renewed = Some(Renewed {
                mono: sent.0,
                wall_ms: sent.1,
            });
        }
        drop(mode);
        self.changed.notify_waiters();
    }

    /// The epoch the worker holds `source` at, while its fence is open. The
    /// tracker will fence state and record writes with it.
    #[allow(dead_code)]
    pub(crate) fn epoch(&self, source: &str) -> Option<u64> {
        let mode = self.mode.lock().expect("lessee lock");
        let Mode::Leased { held, renewed, .. } = &*mode else {
            return None;
        };
        let h = held.get(source)?;
        self.left(*renewed, h.until).map(|_| h.epoch)
    }

    /// Whether a poll may run now: file-managed, or holding a lease inside
    /// the fence. A poll that may not run makes no tracker or vendor call.
    pub(crate) fn may_poll(&self) -> bool {
        match &*self.mode.lock().expect("lessee lock") {
            Mode::Waiting => false,
            Mode::FileManaged => true,
            Mode::Leased { held, renewed, .. } => {
                !held.is_empty() && self.left(*renewed, None).is_some()
            }
        }
    }

    /// How long the fence stays open after `renewed` for a lease good for
    /// `until`, or none once closed. Measured on both clocks, whichever has
    /// run further.
    fn left(&self, renewed: Option<Renewed>, until: Option<Duration>) -> Option<Duration> {
        let r = renewed?;
        let mono = r.mono.elapsed();
        let wall = Duration::from_millis((self.wall)().saturating_sub(r.wall_ms));
        let limit = until.map_or(FENCE, |u| u.min(FENCE));
        limit.checked_sub(mono.max(wall)).filter(|d| !d.is_zero())
    }

    /// How long a vendor call for `repos` may run, none for no limit, or why
    /// it may not start. With leases, nothing runs once the last successful
    /// renew is 60 s old by either clock, and a call into a repo needs the
    /// lease of the connection source naming it. A repo the connection does
    /// not name passes only when `elsewhere` allows it: a read of another
    /// repo's pull request. `None` for repos is a call tied to no repo.
    fn open_for(
        &self,
        repos: Option<&[String]>,
        elsewhere: bool,
    ) -> Result<Option<Duration>, &'static str> {
        match &*self.mode.lock().expect("lessee lock") {
            Mode::FileManaged => Ok(None),
            Mode::Waiting => Err("the config has not answered yet"),
            Mode::Leased {
                sources,
                held,
                renewed,
            } => {
                let mut left = self.left(*renewed, None).ok_or(
                    "no lease renew in the last 60 s; no upstream call until one succeeds",
                )?;
                for r in repos.into_iter().flatten() {
                    let Some(s) = self.source_for(sources, r) else {
                        if elsewhere {
                            continue;
                        }
                        return Err("the worker's connection does not name this repo");
                    };
                    let h = held
                        .get(s)
                        .ok_or("another worker holds this source's lease")?;
                    let l = self
                        .left(*renewed, h.until)
                        .ok_or("this source's lease has run out")?;
                    left = left.min(l);
                }
                Ok(Some(left))
            }
        }
    }

    /// The fence for one call into `repo`, or into no repo.
    pub(crate) fn check(&self, repo: Option<&str>) -> Result<(), SourceError> {
        self.check_in(repo, false)
    }

    fn check_in(&self, repo: Option<&str>, elsewhere: bool) -> Result<(), SourceError> {
        let repos = repo.map(|r| vec![r.to_owned()]);
        self.open_for(repos.as_deref(), elsewhere)
            .map(|_| ())
            .map_err(fenced)
    }

    /// Whether the connection names `repo`. Only a leased worker has one.
    fn names(&self, repo: &str) -> bool {
        match &*self.mode.lock().expect("lessee lock") {
            Mode::Leased { sources, .. } => self.source_for(sources, repo).is_some(),
            _ => true,
        }
    }

    /// Resolves once the fence closes for `repos`: the deadline passes, a
    /// heartbeat drops one of them, or the wall clock shows a suspend.
    async fn closes(&self, repos: Option<&[String]>, elsewhere: bool) -> &'static str {
        loop {
            let changed = self.changed.notified();
            let wait = match self.open_for(repos, elsewhere) {
                Err(why) => return why,
                Ok(left) => left.map_or(RECHECK, |l| l.min(RECHECK)),
            };
            tokio::select! {
                () = changed => {}
                () = tokio::time::sleep(wait) => {}
            }
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

/// The fence refused a request or cut it short. A cut request may still
/// have landed upstream, so its outcome is unknown, not failed.
#[derive(Debug)]
pub(crate) struct FenceClosed(pub(crate) String);

impl std::fmt::Display for FenceClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "lease fence: {}", self.0)
    }
}

impl std::error::Error for FenceClosed {}

/// Whether `e` came from the fence.
pub(crate) fn is_fenced(e: &SourceError) -> bool {
    match e {
        SourceError::Failed(e) => e.chain().any(|c| c.is::<FenceClosed>()),
        SourceError::RateLimited { .. } => false,
    }
}

fn fenced(why: &str) -> SourceError {
    SourceError::Failed(FenceClosed(why.to_owned()).into())
}

tokio::task_local! {
    /// The repos the vendor calls of the current source operation are for,
    /// set by [`Fenced`] and read by [`Gate`].
    static SCOPE: Scope;
}

#[derive(Clone)]
enum Scope {
    Repos(Arc<[String]>),
    /// A read of a pull request, or its releases, in a repo the connection
    /// may not name. Only `GET` passes for a repo outside it.
    Reads(Arc<[String]>),
    /// The worker's own identity, read once at start before any lease. Tied
    /// to no source.
    Identity,
}

/// Runs `f` with its vendor calls tied to no source: the start-up identity
/// read, before the config has answered.
pub(crate) async fn identity<F: Future>(f: F) -> F::Output {
    SCOPE.scope(Scope::Identity, f).await
}

async fn scoped<F: Future>(repos: Vec<String>, f: F) -> F::Output {
    SCOPE.scope(Scope::Repos(repos.into()), f).await
}

async fn reading<F: Future>(repos: Vec<String>, f: F) -> F::Output {
    SCOPE.scope(Scope::Reads(repos.into()), f).await
}

/// The HTTP under a source, fenced. Every vendor request passes here, so a
/// paged listing stops at the fence between pages, and a request in flight
/// is cancelled when the fence closes rather than running out a 30 s
/// timeout.
pub(crate) struct Gate<T> {
    inner: T,
    lessee: Arc<Lessee>,
}

/// `inner` behind `lessee`'s fence.
pub(crate) fn gated<T>(inner: T, lessee: Arc<Lessee>) -> Gate<T> {
    Gate { inner, lessee }
}

impl<T: Transport> Transport for Gate<T> {
    async fn send(&self, req: Req) -> Result<Res> {
        let (repos, elsewhere) = match SCOPE.try_with(Clone::clone) {
            Ok(Scope::Identity) => return self.inner.send(req).await,
            Ok(Scope::Repos(r)) => (Some(r), false),
            // A write never passes outside the connection.
            Ok(Scope::Reads(r)) => (Some(r), req.method == "GET"),
            Err(_) => (None, false),
        };
        let repos = repos.as_deref();
        if let Err(why) = self.lessee.open_for(repos, elsewhere) {
            return Err(FenceClosed(why.to_owned()).into());
        }
        tokio::select! {
            biased;
            why = self.lessee.closes(repos, elsewhere) => {
                Err(FenceClosed(format!("{why}; request cancelled")).into())
            }
            res = self.inner.send(req) => res,
        }
    }
}

/// The tracker calls the two lease tasks make.
pub(crate) trait LeaseApi: Send + Sync {
    /// `PUT /v1/ingest/leases`, reporting the sources `held` now: the leases
    /// held after it, by source.
    fn heartbeat(
        &self,
        group: &str,
        instance: &str,
        leave: bool,
        held: &[String],
    ) -> impl Future<Output = Result<BTreeMap<String, Held>>> + Send;

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
    let held = lessee.held();
    let sent = lessee.sending();
    match api.heartbeat(group, instance, false, &held).await {
        Ok(leases) => lessee.renewed(sent, leases),
        Err(e) => eprintln!("kbl-ingest: lease heartbeat failed: {e:#}"),
    }
}

/// The wait before the next renew: 30 s give or take 20%, so workers that
/// started together do not all renew in the same instant.
fn heartbeat_gap() -> Duration {
    use ring::rand::SecureRandom as _;
    let mut b = [0u8; 2];
    let r = ring::rand::SystemRandom::new()
        .fill(&mut b)
        .map_or(0.5, |()| {
            f64::from(u16::from_le_bytes(b)) / f64::from(u16::MAX)
        });
    HEARTBEAT.mul_f64(0.8 + 0.4 * r)
}

/// Renews about every 30 s, apart from polling, and at once when the config
/// moves the worker to leases.
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
            () = tokio::time::sleep(heartbeat_gap()) => {}
            () = became, if !was => {}
        }
    }
}

/// Stops the heartbeat, then releases the worker's leases. The heartbeat is
/// waited out first: a renew landing after the leave would take them back.
pub(crate) async fn leave<A: LeaseApi>(
    api: &A,
    beating: tokio::task::JoinHandle<()>,
    group: &str,
    instance: &str,
) {
    beating.abort();
    let _ = beating.await;
    if let Err(e) = api.heartbeat(group, instance, true, &[]).await {
        eprintln!("kbl-ingest: could not release the leases: {e:#}");
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
        held: &[String],
    ) -> Result<BTreeMap<String, Held>> {
        let body = json!({ "group": group, "instance": instance, "leave": leave, "held": held });
        let (status, body) = self.put_json("/v1/ingest/leases", &body).await?;
        if status != 200 {
            bail!("the tracker answered {status} to the lease heartbeat: {body}");
        }
        Ok(body["leases"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| {
                let held = Held {
                    epoch: l["epoch"].as_u64()?,
                    until: l["until_ms"].as_u64().map(Duration::from_millis),
                };
                Some((l["source"].as_str()?.to_owned(), held))
            })
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

/// A source whose every operation passes the fence first, and ties the
/// vendor requests it makes to its repos, so [`Gate`] checks each one.
pub(crate) struct Fenced<S> {
    inner: S,
    lessee: Arc<Lessee>,
}

impl<S> Fenced<S> {
    pub(crate) fn new(inner: S, lessee: Arc<Lessee>) -> Self {
        Self { inner, lessee }
    }
}

/// A read about a pull request in `repo`, which an agent may have opened
/// in a repo the connection does not name. In one it names, the lease
/// is needed as for any call; outside it, only reads pass the gate.
async fn read_elsewhere<T>(
    lessee: &Lessee,
    repo: &str,
    f: impl Future<Output = Result<T, SourceError>>,
) -> Result<T, SourceError> {
    if lessee.names(repo) {
        lessee.check(Some(repo))?;
        scoped(vec![repo.to_owned()], f).await
    } else {
        lessee.check_in(Some(repo), true)?;
        reading(vec![repo.to_owned()], f).await
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

    /// Lists only the repos whose leases the worker holds; a repo the
    /// connection does not name is never listed.
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
        scoped(repos.clone(), self.inner.changed(&repos, cursors)).await
    }

    async fn pull_request(&self, id: &IssueRef) -> Result<Option<PullRequest>, SourceError> {
        read_elsewhere(&self.lessee, &id.repo, self.inner.pull_request(id)).await
    }

    async fn fetch(&self, id: &IssueRef) -> Result<Fetched, SourceError> {
        self.lessee.check(Some(&id.repo))?;
        scoped(vec![id.repo.to_owned()], self.inner.fetch(id)).await
    }

    async fn issue_comments(&self, id: &IssueRef) -> Result<Vec<Comment>, SourceError> {
        self.lessee.check(Some(&id.repo))?;
        scoped(vec![id.repo.to_owned()], self.inner.issue_comments(id)).await
    }

    async fn links(&self, id: &IssueRef) -> Result<Links, SourceError> {
        self.lessee.check(Some(&id.repo))?;
        scoped(vec![id.repo.to_owned()], self.inner.links(id)).await
    }

    async fn write_back(&self, what: &WriteBack) -> Result<(), SourceError> {
        // Checked here and again by the gate right before the request, as a
        // write-back is the one upstream call a second worker could repeat.
        self.lessee.check(Some(written_repo(what)))?;
        scoped(
            vec![written_repo(what).to_owned()],
            self.inner.write_back(what),
        )
        .await
    }

    async fn releases(&self, repo: &str, since_ms: u64) -> Result<Releases, SourceError> {
        read_elsewhere(&self.lessee, repo, self.inner.releases(repo, since_ms)).await
    }

    async fn contains(&self, repo: &str, tag: &str, sha: &str) -> Result<bool, SourceError> {
        read_elsewhere(&self.lessee, repo, self.inner.contains(repo, tag, sha)).await
    }

    async fn open_issue(
        &self,
        repo: &str,
        title: &str,
        body: &str,
    ) -> Result<IssueRef, SourceError> {
        self.lessee.check(Some(repo))?;
        scoped(
            vec![repo.to_owned()],
            self.inner.open_issue(repo, title, body),
        )
        .await
    }
}

#[cfg(test)]
mod tests;
