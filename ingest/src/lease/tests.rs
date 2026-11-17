use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use kbl_core::lease::{self as rules, Lease};

use super::*;

/// A source that counts the vendor calls that reach it, and the repos the
/// last listing asked for.
#[derive(Default)]
struct Counting(AtomicU32, Mutex<Vec<String>>);

impl Counting {
    fn calls(&self) -> u32 {
        self.0.load(Ordering::SeqCst)
    }
    fn hit(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl Source for Counting {
    fn name(&self) -> &'static str {
        "github"
    }
    fn own_logins(&self) -> Vec<String> {
        Vec::new()
    }
    async fn changed(
        &self,
        repos: &[String],
        _: &BTreeMap<String, Cursor>,
    ) -> Result<Changes, SourceError> {
        self.hit();
        *self.1.lock().unwrap() = repos.to_vec();
        Ok(Changes::default())
    }
    async fn pull_request(&self, _: &IssueRef) -> Result<Option<PullRequest>, SourceError> {
        self.hit();
        Ok(None)
    }
    async fn fetch(&self, _: &IssueRef) -> Result<Fetched, SourceError> {
        self.hit();
        Ok(Fetched::Gone)
    }
    async fn issue_comments(&self, _: &IssueRef) -> Result<Vec<Comment>, SourceError> {
        self.hit();
        Ok(Vec::new())
    }
    async fn links(&self, _: &IssueRef) -> Result<Links, SourceError> {
        self.hit();
        Ok(Links::default())
    }
    async fn write_back(&self, _: &WriteBack) -> Result<(), SourceError> {
        self.hit();
        Ok(())
    }
    async fn releases(&self, _: &str, _: u64) -> Result<Releases, SourceError> {
        self.hit();
        Ok(Releases::default())
    }
    async fn contains(&self, _: &str, _: &str, _: &str) -> Result<bool, SourceError> {
        self.hit();
        Ok(false)
    }
    async fn open_issue(&self, repo: &str, _: &str, _: &str) -> Result<IssueRef, SourceError> {
        self.hit();
        Ok(IssueRef::new(repo, 1))
    }
}

fn leased(sources: &[&str]) -> Arc<Lessee> {
    leased_with_suspend(sources).0
}

/// A leased worker whose wall clock follows the paused clock, plus what the
/// returned counter adds: time the machine spent suspended, which the
/// monotonic clock does not see.
fn leased_with_suspend(sources: &[&str]) -> (Arc<Lessee>, Arc<AtomicU64>) {
    let start = Instant::now();
    let slept = Arc::new(AtomicU64::new(0));
    let s = slept.clone();
    let wall: Wall =
        Box::new(move || 1_000_000 + start.elapsed().as_millis() as u64 + s.load(Ordering::SeqCst));
    let l = Lessee::new("github", Mode::Waiting, wall);
    l.configure(&json!({ "sources": sources }));
    (l, slept)
}

fn held(pairs: &[(&str, u64)]) -> BTreeMap<String, Held> {
    pairs
        .iter()
        .map(|(s, e)| {
            (
                (*s).to_owned(),
                Held {
                    epoch: *e,
                    until: None,
                },
            )
        })
        .collect()
}

fn issue(repo: &str) -> IssueRef {
    IssueRef::new(repo, 7)
}

fn comment_on(repo: &str) -> WriteBack {
    WriteBack::Comment {
        issue: issue(repo),
        body: "b".into(),
    }
}

#[tokio::test(start_paused = true)]
async fn the_fence_stops_vendor_calls_and_write_backs_60_s_after_the_last_renew() {
    let lessee = leased(&["github:o/a"]);
    let src = Fenced::new(Counting::default(), lessee.clone());
    lessee.renewed(lessee.sending(), held(&[("github:o/a", 1)]));
    assert_eq!(lessee.epoch("github:o/a"), Some(1));
    src.fetch(&issue("o/a")).await.unwrap();
    tokio::time::advance(Duration::from_secs(59)).await;
    src.write_back(&comment_on("o/a")).await.unwrap();
    assert_eq!(src.inner.calls(), 2);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(src.fetch(&issue("o/a")).await.is_err());
    assert!(src.write_back(&comment_on("o/a")).await.is_err());
    assert!(
        src.changed(&["o/a".into()], &BTreeMap::new())
            .await
            .is_err()
    );
    assert!(src.releases("o/a", 0).await.is_err());
    assert!(
        src.pull_request(&issue("o/other")).await.is_err(),
        "any repo"
    );
    assert_eq!(src.inner.calls(), 2, "nothing reached the vendor");
    assert!(!lessee.may_poll());
    assert_eq!(lessee.epoch("github:o/a"), None);
    // A renew that succeeds opens it again.
    lessee.renewed(lessee.sending(), held(&[("github:o/a", 1)]));
    src.write_back(&comment_on("o/a")).await.unwrap();
    assert_eq!(src.inner.calls(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_worker_polls_and_writes_only_the_sources_it_leases() {
    let lessee = leased(&["github:o/a", "github:o/b"]);
    let src = Fenced::new(Counting::default(), lessee.clone());
    lessee.renewed(lessee.sending(), held(&[("github:o/a", 3)]));
    let repos = ["o/a".to_owned(), "o/b".to_owned(), "o/elsewhere".to_owned()];
    src.changed(&repos, &BTreeMap::new()).await.unwrap();
    assert_eq!(*src.inner.1.lock().unwrap(), ["o/a", "o/elsewhere"]);
    assert!(src.write_back(&comment_on("o/b")).await.is_err());
    assert!(src.fetch(&issue("o/b")).await.is_err());
    // A repo no connection source names, such as another source's pull
    // request, needs only the fence.
    src.pull_request(&issue("o/elsewhere")).await.unwrap();
    // A heartbeat that no longer lists o/a stops it at once.
    lessee.renewed(lessee.sending(), BTreeMap::new());
    assert!(src.fetch(&issue("o/a")).await.is_err());
    assert!(!lessee.may_poll());
}

#[tokio::test(start_paused = true)]
async fn a_worker_with_no_connection_is_never_fenced_and_one_waiting_for_config_is() {
    let file = Lessee::file_managed("github");
    let src = Fenced::new(Counting::default(), file.clone());
    tokio::time::advance(Duration::from_secs(3600)).await;
    src.write_back(&comment_on("o/a")).await.unwrap();
    src.changed(&["o/a".into()], &BTreeMap::new())
        .await
        .unwrap();
    assert!(file.may_poll());

    let waiting = Lessee::waiting("github");
    let src = Fenced::new(Counting::default(), waiting.clone());
    assert!(src.fetch(&issue("o/a")).await.is_err());
    assert!(!waiting.may_poll());
    // A config answer with no connection makes it file-managed.
    waiting.configure(&Value::Null);
    src.fetch(&issue("o/a")).await.unwrap();
}

/// Trackers over one lease store, each with its own clock offset, that grant
/// by the shared lease rules on the paused clock and never hear from the
/// instances in `cut`.
struct Trackers {
    start: Instant,
    leases: Mutex<HashMap<String, Lease>>,
    cut: Mutex<Vec<String>>,
    /// Instance to how far ahead the clock of the tracker it reaches runs.
    skew_ms: HashMap<&'static str, u64>,
    /// Heartbeats answered, with whether they were a leave.
    beats: Mutex<Vec<(String, bool)>>,
    /// How long a heartbeat takes to answer.
    slow: Duration,
}

impl Trackers {
    fn new(skew_ms: &[(&'static str, u64)]) -> Self {
        Self {
            start: Instant::now(),
            leases: Mutex::default(),
            cut: Mutex::default(),
            skew_ms: skew_ms.iter().copied().collect(),
            beats: Mutex::default(),
            slow: Duration::ZERO,
        }
    }
}

impl LeaseApi for Trackers {
    async fn heartbeat(
        &self,
        _: &str,
        me: &str,
        leave: bool,
        _: &[String],
    ) -> Result<BTreeMap<String, Held>> {
        tokio::time::sleep(self.slow).await;
        if self.cut.lock().unwrap().iter().any(|c| c == me) {
            bail!("partitioned");
        }
        self.beats.lock().unwrap().push((me.to_owned(), leave));
        let now =
            self.start.elapsed().as_millis() as u64 + self.skew_ms.get(me).copied().unwrap_or(0);
        let mut leases = self.leases.lock().unwrap();
        let current = leases.get("github:o/a").cloned();
        let mut got = BTreeMap::new();
        if leave {
            if let Some(l) = current.filter(|l| l.holder.as_deref() == Some(me)) {
                let released = Lease {
                    holder: None,
                    expires_ms: now,
                    ..l
                };
                leases.insert("github:o/a".to_owned(), released);
            }
            return Ok(got);
        }
        let step = rules::step(current.as_ref(), me, true, now);
        if let Some(next) = rules::next(current.as_ref(), me, step, now) {
            got.insert(
                "github:o/a".to_owned(),
                Held {
                    epoch: next.epoch,
                    until: None,
                },
            );
            leases.insert("github:o/a".to_owned(), next);
        }
        Ok(got)
    }

    async fn config(
        &self,
        _: &str,
        _: &str,
        _: Option<&str>,
        _: u64,
    ) -> Result<Option<(Value, String)>> {
        Ok(None)
    }
}

/// A vendor whose every request is busy on its side for `busy`, from when it
/// is sent, whether or not the caller gives up on it sooner. It logs who was
/// busy with it and when, in seconds of the paused clock.
struct Vendor {
    start: Instant,
    busy: Duration,
    log: Mutex<Vec<(&'static str, f64, f64)>>,
}

impl Vendor {
    fn new(busy: Duration) -> Arc<Self> {
        Arc::new(Self {
            start: Instant::now(),
            busy,
            log: Mutex::default(),
        })
    }

    fn of(&self, who: &str) -> Vec<(f64, f64)> {
        let log = self.log.lock().unwrap();
        log.iter()
            .filter(|(w, ..)| *w == who)
            .map(|(_, a, b)| (*a, *b))
            .collect()
    }
}

/// One worker's connection to the [`Vendor`].
struct Line(Arc<Vendor>, &'static str);

impl Transport for Line {
    async fn send(&self, _: Req) -> Result<Res> {
        let v = &self.0;
        let at = v.start.elapsed().as_secs_f64();
        v.log
            .lock()
            .unwrap()
            .push((self.1, at, at + v.busy.as_secs_f64()));
        tokio::time::sleep(v.busy).await;
        Ok(Res {
            status: 200,
            headers: BTreeMap::new(),
            body: Value::Null,
        })
    }
}

/// An adapter over a fenced line: a listing pages through `pages` requests,
/// and every other call is one request.
struct Paging<T> {
    http: T,
    pages: u32,
}

impl<T: Transport> Paging<T> {
    async fn one(&self) -> Result<(), SourceError> {
        self.http
            .send(Req::get("https://vendor.test/x".into()))
            .await
            .map(|_| ())
            .map_err(SourceError::Failed)
    }
}

impl<T: Transport> Source for Paging<T> {
    fn name(&self) -> &'static str {
        "github"
    }
    fn own_logins(&self) -> Vec<String> {
        Vec::new()
    }
    async fn changed(
        &self,
        _: &[String],
        _: &BTreeMap<String, Cursor>,
    ) -> Result<Changes, SourceError> {
        for _ in 0..self.pages {
            self.one().await?;
        }
        Ok(Changes::default())
    }
    async fn pull_request(&self, _: &IssueRef) -> Result<Option<PullRequest>, SourceError> {
        self.one().await.map(|()| None)
    }
    async fn fetch(&self, _: &IssueRef) -> Result<Fetched, SourceError> {
        self.one().await.map(|()| Fetched::Gone)
    }
    async fn issue_comments(&self, _: &IssueRef) -> Result<Vec<Comment>, SourceError> {
        self.one().await.map(|()| Vec::new())
    }
    async fn links(&self, _: &IssueRef) -> Result<Links, SourceError> {
        self.one().await.map(|()| Links::default())
    }
    async fn write_back(&self, _: &WriteBack) -> Result<(), SourceError> {
        self.one().await
    }
    async fn releases(&self, _: &str, _: u64) -> Result<Releases, SourceError> {
        self.one().await.map(|()| Releases::default())
    }
    async fn contains(&self, _: &str, _: &str, _: &str) -> Result<bool, SourceError> {
        self.one().await.map(|()| false)
    }
    async fn open_issue(&self, repo: &str, _: &str, _: &str) -> Result<IssueRef, SourceError> {
        self.one().await.map(|()| IssueRef::new(repo, 1))
    }
}

fn fenced_vendor(
    vendor: &Arc<Vendor>,
    who: &'static str,
    lessee: &Arc<Lessee>,
    pages: u32,
) -> Fenced<Paging<Gate<Line>>> {
    let http = gated(Line(vendor.clone(), who), lessee.clone());
    Fenced::new(Paging { http, pages }, lessee.clone())
}

/// Polls in a loop, as the worker does: a listing whenever the fence lets it.
async fn poll_loop<S: Source>(src: Arc<Fenced<S>>, lessee: Arc<Lessee>) {
    loop {
        if lessee.may_poll() {
            let _ = src.changed(&["o/a".into()], &BTreeMap::new()).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn after_a_partition_no_two_workers_are_busy_upstream_for_one_source_at_once() {
    // A's renews reach a tracker on time; B's reach one whose clock runs 30 s
    // ahead, the most the trackers may disagree by. Each vendor request is
    // busy for 19 s, so a listing pages past A's fence and A's last request
    // is still busy upstream after its fence closes.
    let api = Arc::new(Trackers::new(&[("A", 0), ("B", 30_000)]));
    let vendor = Vendor::new(Duration::from_secs(19));
    let (a, b) = (leased(&["github:o/a"]), leased(&["github:o/a"]));
    let src_a = Arc::new(fenced_vendor(&vendor, "A", &a, 6));
    let src_b = Arc::new(fenced_vendor(&vendor, "B", &b, 6));
    heartbeat_once(&*api, &a, "g", "A").await;
    api.cut.lock().unwrap().push("A".into());
    let tasks = [
        tokio::spawn(heartbeat_task(
            api.clone(),
            a.clone(),
            "g".into(),
            "A".into(),
        )),
        tokio::spawn(poll_loop(src_a, a.clone())),
        tokio::spawn(heartbeat_task(
            api.clone(),
            b.clone(),
            "g".into(),
            "B".into(),
        )),
        tokio::spawn(poll_loop(src_b, b.clone())),
    ];
    tokio::time::sleep(Duration::from_secs(240)).await;
    for t in tasks {
        t.abort();
    }
    let (on_a, on_b) = (vendor.of("A"), vendor.of("B"));
    assert!(
        !on_a.is_empty() && !on_b.is_empty(),
        "A {on_a:?}, B {on_b:?}"
    );
    for (a0, a1) in &on_a {
        assert!(*a0 < 60.0, "A sent a request at {a0} s, past its fence");
        for (b0, b1) in &on_b {
            assert!(
                a1 <= b0 || b1 <= a0,
                "A busy {a0}..{a1} s and B busy {b0}..{b1} s at once"
            );
        }
    }
    assert_eq!(
        b.epoch("github:o/a"),
        Some(2),
        "the takeover bumps the epoch"
    );
}

#[tokio::test(start_paused = true)]
async fn a_request_in_flight_is_cancelled_when_the_fence_closes() {
    let lessee = leased(&["github:o/a"]);
    let vendor = Vendor::new(Duration::from_secs(50));
    let src = fenced_vendor(&vendor, "A", &lessee, 1);
    lessee.renewed(lessee.sending(), held(&[("github:o/a", 1)]));
    tokio::time::advance(Duration::from_secs(30)).await;
    let started = Instant::now();
    let err = src.fetch(&issue("o/a")).await.unwrap_err();
    assert!(err.to_string().contains("lease fence"), "{err}");
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(30),
        "cut at the 60 s fence"
    );
}

#[tokio::test(start_paused = true)]
async fn a_suspend_closes_the_fence_though_the_monotonic_clock_stood_still() {
    let (lessee, slept) = leased_with_suspend(&["github:o/a"]);
    let vendor = Vendor::new(Duration::from_secs(50));
    let src = Arc::new(fenced_vendor(&vendor, "A", &lessee, 1));
    lessee.renewed(lessee.sending(), held(&[("github:o/a", 1)]));
    let in_flight = tokio::spawn({
        let src = src.clone();
        async move { src.fetch(&issue("o/a")).await }
    });
    tokio::time::advance(Duration::from_secs(10)).await;
    // The machine sleeps for two minutes: only the wall clock moves.
    slept.store(120_000, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    let cut = tokio::time::timeout(Duration::from_secs(2), in_flight)
        .await
        .expect("cut within a second of waking")
        .unwrap();
    assert!(cut.is_err());
    assert!(!lessee.may_poll());
    assert!(src.fetch(&issue("o/a")).await.is_err());
    assert_eq!(vendor.of("A").len(), 1, "no request after the suspend");
}

#[tokio::test(start_paused = true)]
async fn a_moved_source_is_polled_only_until_its_lease_runs_out() {
    let lessee = leased(&["github:o/a", "github:o/b"]);
    let src = Fenced::new(Counting::default(), lessee.clone());
    let mut leases = held(&[("github:o/a", 1)]);
    leases.insert(
        "github:o/b".into(),
        Held {
            epoch: 4,
            until: Some(Duration::from_secs(20)),
        },
    );
    lessee.renewed(lessee.sending(), leases);
    src.fetch(&issue("o/b")).await.unwrap();
    tokio::time::advance(Duration::from_secs(21)).await;
    assert!(src.fetch(&issue("o/b")).await.is_err());
    assert_eq!(lessee.epoch("github:o/b"), None);
    src.fetch(&issue("o/a")).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_request_outside_any_operation_still_passes_the_fence() {
    let lessee = leased(&["github:o/a"]);
    let vendor = Vendor::new(Duration::from_secs(1));
    let gate = gated(Line(vendor.clone(), "A"), lessee.clone());
    let req = || Req::get("https://vendor.test/user".into());
    assert!(gate.send(req()).await.is_err(), "no renew yet");
    // The start-up identity read is the one call tied to no source.
    identity(gate.send(req())).await.unwrap();
    lessee.renewed(lessee.sending(), held(&[("github:o/a", 1)]));
    gate.send(req()).await.unwrap();
    assert!(
        scoped(vec!["o/a".into()], gate.send(req())).await.is_ok()
            && scoped(vec!["o/b".into()], gate.send(req())).await.is_ok(),
        "o/b is no connection source"
    );
    lessee.renewed(lessee.sending(), BTreeMap::new());
    assert!(scoped(vec!["o/a".into()], gate.send(req())).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn a_leaving_worker_stops_its_heartbeat_before_it_releases() {
    let mut api = Trackers::new(&[]);
    // A renew is in flight when the worker stops.
    api.slow = Duration::from_secs(5);
    let api = Arc::new(api);
    let lessee = leased(&["github:o/a"]);
    let beating = tokio::spawn(heartbeat_task(
        api.clone(),
        lessee.clone(),
        "g".into(),
        "A".into(),
    ));
    tokio::time::sleep(Duration::from_secs(32)).await;
    leave(&*api, beating, "g", "A").await;
    tokio::time::sleep(Duration::from_secs(120)).await;
    let beats = api.beats.lock().unwrap().clone();
    assert_eq!(
        beats.last(),
        Some(&("A".into(), true)),
        "a renew after the leave: {beats:?}"
    );
    let l = api.leases.lock().unwrap()["github:o/a"].clone();
    assert_eq!(l.holder, None, "released, not taken back");
}
