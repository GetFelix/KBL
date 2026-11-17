use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

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
    let l = Lessee::waiting("github");
    l.configure(&json!({ "sources": sources }));
    l
}

fn held(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
    pairs.iter().map(|(s, e)| ((*s).to_owned(), *e)).collect()
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
    lessee.renewed(Instant::now(), held(&[("github:o/a", 1)]));
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
    lessee.renewed(Instant::now(), held(&[("github:o/a", 1)]));
    src.write_back(&comment_on("o/a")).await.unwrap();
    assert_eq!(src.inner.calls(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_worker_polls_and_writes_only_the_sources_it_leases() {
    let lessee = leased(&["github:o/a", "github:o/b"]);
    let src = Fenced::new(Counting::default(), lessee.clone());
    lessee.renewed(Instant::now(), held(&[("github:o/a", 3)]));
    let repos = ["o/a".to_owned(), "o/b".to_owned(), "o/elsewhere".to_owned()];
    src.changed(&repos, &BTreeMap::new()).await.unwrap();
    assert_eq!(*src.inner.1.lock().unwrap(), ["o/a", "o/elsewhere"]);
    assert!(src.write_back(&comment_on("o/b")).await.is_err());
    assert!(src.fetch(&issue("o/b")).await.is_err());
    // A repo no connection source names, such as another source's pull
    // request, needs only the fence.
    src.pull_request(&issue("o/elsewhere")).await.unwrap();
    // A heartbeat that no longer lists o/a stops it at once.
    lessee.renewed(Instant::now(), BTreeMap::new());
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

/// A tracker that grants by the shared lease rules on the paused clock, and
/// never hears from the instances in `cut`.
struct Tracker {
    start: Instant,
    leases: Mutex<HashMap<String, Lease>>,
    cut: Mutex<Vec<String>>,
}

impl LeaseApi for Tracker {
    async fn heartbeat(&self, _: &str, me: &str, _: bool) -> Result<BTreeMap<String, u64>> {
        if self.cut.lock().unwrap().iter().any(|c| c == me) {
            bail!("partitioned");
        }
        let now = self.start.elapsed().as_millis() as u64;
        let mut leases = self.leases.lock().unwrap();
        let current = leases.get("github:o/a").cloned();
        let step = rules::step(current.as_ref(), me, true, now);
        let mut got = BTreeMap::new();
        if let Some(next) = rules::next(current.as_ref(), me, step, now) {
            got.insert("github:o/a".to_owned(), next.epoch);
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

#[tokio::test(start_paused = true)]
async fn after_a_partition_the_old_holder_stops_before_the_new_one_starts() {
    let api = Tracker {
        start: Instant::now(),
        leases: Mutex::default(),
        cut: Mutex::default(),
    };
    let (a, b) = (leased(&["github:o/a"]), leased(&["github:o/a"]));
    let src_a = Fenced::new(Counting::default(), a.clone());
    let src_b = Fenced::new(Counting::default(), b.clone());
    let (mut a_calls, mut b_calls) = (Vec::new(), Vec::new());
    for second in 0..240u64 {
        // A takes the lease at 0 s, then nothing it sends arrives.
        if second == 1 {
            api.cut.lock().unwrap().push("A".into());
        }
        if second % 30 == 0 {
            heartbeat_once(&api, &a, "g", "A").await;
        }
        if second % 30 == 1 {
            heartbeat_once(&api, &b, "g", "B").await;
        }
        if src_a.fetch(&issue("o/a")).await.is_ok() {
            a_calls.push(second);
        }
        if src_b.write_back(&comment_on("o/a")).await.is_ok() {
            b_calls.push(second);
        }
        tokio::time::advance(Duration::from_secs(1)).await;
    }
    let a_last = *a_calls.last().unwrap();
    let b_first = *b_calls.first().unwrap();
    assert!(a_last <= 60, "A called upstream at {a_last} s");
    assert!(b_first >= 90, "B took over at {b_first} s");
    assert_eq!(
        b.epoch("github:o/a"),
        Some(2),
        "the takeover bumps the epoch"
    );
}
