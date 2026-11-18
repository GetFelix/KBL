use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::body::Body;
use axum::http::Request;
use kbl_core::org::TeamName;
use tower::ServiceExt;

use super::*;
use crate::config::Tokens;
use crate::felix::ConditionalWrite;
use crate::teams::Team;

mod metrics;
mod org;
mod root;
mod traces;

/// A Felix with no records that remembers which streams were published to, and
/// whose writer lease can be taken away.
#[derive(Clone, Default)]
struct Quiet {
    published: Arc<StdMutex<Vec<String>>>,
    lease_lost: Arc<AtomicBool>,
    /// What every read returns, for a log that is there before the start.
    log: Arc<StdMutex<Records>>,
    /// Publishes also go on `log`, so a reread finds them.
    keeps: Arc<AtomicBool>,
    /// Publishes fail, and are not recorded.
    fail_publish: Arc<AtomicBool>,
    /// A renewal by version is refused once the lease has run out, as Felix does.
    lease_expires: Arc<AtomicBool>,
    lease_until: Arc<StdMutex<Option<tokio::time::Instant>>>,
    /// How much longer than its time to live the entry stands: Felix starts it
    /// when the request lands, after the tracker started its own clock.
    lease_lag: Arc<StdMutex<Duration>>,
    /// Bumped on every grant, as Felix does.
    lease_version: Arc<std::sync::atomic::AtomicU64>,
    /// Who the last grant went to.
    lease_holder: Arc<StdMutex<Vec<u8>>>,
    /// The next lease put lands, but its reply is lost.
    lose_lease_reply: Arc<AtomicBool>,
    /// Reads of the lease key fail.
    fail_lease_get: Arc<AtomicBool>,
    /// A conditional publish is refused unless it names the next offset.
    fences: Arc<AtomicBool>,
    /// With `fences`, a refusal takes the offset it asked for, as on a Raft
    /// stream shard: the record is never stored, and the tip passes it.
    raft_blanks: Arc<AtomicBool>,
    /// Offsets another writer was given whose records are not committed yet:
    /// Felix's tail counts them, reads do not show them.
    in_flight: Arc<std::sync::atomic::AtomicU64>,
    /// The next read fails.
    fail_next_read: Arc<AtomicBool>,
    /// The next read takes this long. With a record, another tracker meanwhile
    /// takes the lease and appends it.
    slow_read: Arc<StdMutex<Option<SlowRead>>>,
    /// Ingest source leases by source: value and version.
    source_leases: Arc<StdMutex<std::collections::HashMap<String, Versioned>>>,
    /// Ingest worker groups' members by group: value and version.
    group_members: Arc<StdMutex<std::collections::HashMap<String, Versioned>>>,
    /// Membership writes fail.
    fail_members_put: Arc<AtomicBool>,
    /// This many membership writes lose the version check, as when other
    /// trackers write the entry first.
    lose_members_puts: Arc<std::sync::atomic::AtomicU32>,
    /// Another tracker writes this to a source's lease just before the next
    /// lease put, so that put's version check fails.
    race_lease_put: Arc<StdMutex<Option<Vec<u8>>>>,
}

type Records = Vec<(u64, Vec<u8>)>;
/// A cache value and its version.
type Versioned = (Vec<u8>, u64);

/// How long a read takes, and the record another tracker appends meanwhile.
type SlowRead = (Duration, Option<Vec<u8>>);

impl Quiet {
    fn published(&self) -> Vec<String> {
        self.published.lock().unwrap().clone()
    }
}

impl Broker for Quiet {
    async fn publish(&self, stream: &str, payload: Vec<u8>) -> anyhow::Result<u64> {
        if self.fail_publish.load(Ordering::SeqCst) {
            anyhow::bail!("publish refused");
        }
        let mut published = self.published.lock().unwrap();
        published.push(stream.to_owned());
        let offset = published.len() as u64 - 1;
        if self.keeps.load(Ordering::SeqCst) {
            self.log.lock().unwrap().push((offset, payload));
        }
        Ok(offset)
    }
    async fn publish_if(
        &self,
        stream: &str,
        payload: Vec<u8>,
        expected: u64,
    ) -> anyhow::Result<ConditionalWrite> {
        let tail =
            self.published.lock().unwrap().len() as u64 + self.in_flight.load(Ordering::SeqCst);
        if self.fences.load(Ordering::SeqCst) && expected != tail {
            if self.raft_blanks.load(Ordering::SeqCst) {
                self.published.lock().unwrap().push(stream.to_owned());
                return Ok(ConditionalWrite::Refused { tail: tail + 1 });
            }
            return Ok(ConditionalWrite::Refused { tail });
        }
        let offset = self.publish(stream, payload).await?;
        Ok(ConditionalWrite::Written { offset })
    }
    async fn read_from(&self, stream: &str, _: u64) -> anyhow::Result<Vec<(u64, Vec<u8>)>> {
        if self.fail_next_read.swap(false, Ordering::SeqCst) {
            anyhow::bail!("the broker is down");
        }
        let slow = self.slow_read.lock().unwrap().take();
        if let Some((pause, theirs)) = slow {
            tokio::time::sleep(pause).await;
            if let Some(theirs) = theirs {
                self.lease_lost.store(true, Ordering::SeqCst);
                self.publish(stream, theirs).await?;
            }
        }
        Ok(self.log.lock().unwrap().clone())
    }
    async fn read_to_tip(&self, stream: &str, from: u64) -> anyhow::Result<crate::felix::ToTip> {
        let records = self.read_from(stream, from).await?;
        let mut tip = records.last().map_or(from, |(o, _)| o + 1);
        // Felix's empty page points past a trailing blank.
        if self.raft_blanks.load(Ordering::SeqCst) {
            tip = tip.max(self.published.lock().unwrap().len() as u64);
        }
        Ok((records, tip))
    }
    async fn poll(
        &self,
        _: &str,
        _: &str,
        _: Duration,
    ) -> anyhow::Result<Option<felix_client::GroupRecord>> {
        Ok(None)
    }
    async fn ack(&self, _: &str, _: &str, _: u64) -> anyhow::Result<()> {
        Ok(())
    }
    async fn nack(&self, _: &str, _: &str, _: &felix_client::GroupRecord) -> anyhow::Result<()> {
        Ok(())
    }
    async fn extend(
        &self,
        _: &str,
        _: &str,
        _: &felix_client::GroupRecord,
        by: Duration,
    ) -> Result<Duration, crate::felix::ExtendError> {
        Ok(by)
    }
    async fn lease_put(
        &self,
        holder: Vec<u8>,
        ttl: Duration,
        expect: Option<u64>,
    ) -> anyhow::Result<Option<u64>> {
        let now = tokio::time::Instant::now();
        let mut until = self.lease_until.lock().unwrap();
        if self.lease_expires.load(Ordering::SeqCst)
            && expect.is_some()
            && until.is_none_or(|t| t <= now)
        {
            return Ok(None);
        }
        // Felix refuses an Absent put while a live entry exists.
        if expect.is_none() && until.is_some_and(|t| t > now) {
            return Ok(None);
        }
        if self.lease_lost.load(Ordering::SeqCst) {
            return Ok(None);
        }
        // Felix refuses a version that is not the live entry's.
        if expect.is_some_and(|v| v != self.lease_version.load(Ordering::SeqCst)) {
            return Ok(None);
        }
        *until = Some(now + ttl + *self.lease_lag.lock().unwrap());
        *self.lease_holder.lock().unwrap() = holder;
        let version = self.lease_version.fetch_add(1, Ordering::SeqCst) + 1;
        if self.lose_lease_reply.swap(false, Ordering::SeqCst) {
            anyhow::bail!("Felix did not answer in time");
        }
        Ok(Some(version))
    }
    async fn lease_get(&self) -> anyhow::Result<Option<(Vec<u8>, u64)>> {
        if self.fail_lease_get.load(Ordering::SeqCst) {
            anyhow::bail!("the broker is down");
        }
        let version = self.lease_version.load(Ordering::SeqCst);
        // Another tracker holds it.
        if self.lease_lost.load(Ordering::SeqCst) {
            return Ok(Some((b"another tracker".to_vec(), version)));
        }
        let live = self
            .lease_until
            .lock()
            .unwrap()
            .is_some_and(|t| t > tokio::time::Instant::now());
        Ok(live.then(|| (self.lease_holder.lock().unwrap().clone(), version)))
    }
    async fn source_lease_get(&self, source: &str) -> anyhow::Result<Option<(Vec<u8>, u64)>> {
        Ok(self.source_leases.lock().unwrap().get(source).cloned())
    }
    async fn source_lease_put(
        &self,
        source: &str,
        value: Vec<u8>,
        expect: Option<u64>,
    ) -> anyhow::Result<Option<u64>> {
        let mut leases = self.source_leases.lock().unwrap();
        if let Some(other) = self.race_lease_put.lock().unwrap().take() {
            let next = leases.get(source).map_or(1, |(_, v)| v + 1);
            leases.insert(source.to_owned(), (other, next));
        }
        let now = leases.get(source).map(|(_, v)| *v);
        if now != expect {
            return Ok(None);
        }
        let next = now.unwrap_or(0) + 1;
        leases.insert(source.to_owned(), (value, next));
        Ok(Some(next))
    }
    async fn group_members_get(&self, group: &str) -> anyhow::Result<Option<(Vec<u8>, u64)>> {
        // A real read is a round trip, so concurrent heartbeats interleave.
        tokio::task::yield_now().await;
        Ok(self.group_members.lock().unwrap().get(group).cloned())
    }
    async fn group_members_put(
        &self,
        group: &str,
        value: Vec<u8>,
        expect: Option<u64>,
    ) -> anyhow::Result<Option<u64>> {
        if self.fail_members_put.load(Ordering::SeqCst) {
            anyhow::bail!("the cache is unreachable");
        }
        // Lose this write while the injected count lasts.
        let mut left = self.lose_members_puts.load(Ordering::SeqCst);
        while left > 0 {
            match self.lose_members_puts.compare_exchange(
                left,
                left - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Ok(None),
                Err(now) => left = now,
            }
        }
        let mut all = self.group_members.lock().unwrap();
        let now = all.get(group).map(|(_, v)| *v);
        if now != expect {
            return Ok(None);
        }
        let next = now.unwrap_or(0) + 1;
        all.insert(group.to_owned(), (value, next));
        Ok(Some(next))
    }
    async fn forget(&self, _: Option<&str>) {}
    async fn close_idle(&self, _: Duration) {}
}

fn quiet_settings() -> crate::tracker::Settings {
    crate::tracker::Settings {
        grants: Default::default(),
        visibility: Duration::from_secs(90),
        lease_ttl: Duration::from_secs(15),
        lease_wait: Duration::ZERO,
        crash_after_poll: false,
        crash_after_done: false,
        review_tiers: crate::tracker::default_review_tiers(),
        cost_floor_micros: 0,
        team: String::new(),
        review_limit: 0,
        snapshot_dir: None,
        snapshot_every: 0,
        assignee_away: Duration::from_secs(600),
    }
}

async fn team(name: &str, broker: Quiet, tokens: &str, inflight: usize) -> Team<Quiet> {
    let tracker = Tracker::start(
        broker,
        crate::tracker::Settings {
            grants: Default::default(),
            visibility: Duration::from_secs(90),
            lease_ttl: Duration::from_secs(15),
            lease_wait: Duration::ZERO,
            crash_after_poll: false,
            crash_after_done: false,
            review_tiers: crate::tracker::default_review_tiers(),
            cost_floor_micros: 0,
            team: name.to_owned(),
            review_limit: 3,
            snapshot_dir: None,
            snapshot_every: 0,
            assignee_away: Duration::from_secs(600),
        },
    )
    .await
    .unwrap();
    Team::new(
        TeamName::new(name).unwrap(),
        tracker,
        Tokens::parse(tokens).unwrap(),
        inflight,
    )
}

/// A tracker with one team and an empty Felix: enough to exercise auth and routing.
async fn app() -> Router {
    let tokens =
        r#"{"agents":{"tok-w1":"w1"},"people":{"tok-gab":"gab"},"ingest":{"tok-ing":"ingest-gh"}}"#;
    let one = team("default", Quiet::default(), tokens, 64).await;
    router(Arc::new(Teams::new(vec![one]).unwrap()))
}

/// Two teams, `a` and `b`, each with its own Felix. `tok-both` is one person in both.
struct Two {
    app: Router,
    teams: Arc<Teams<Quiet>>,
    a: Quiet,
    b: Quiet,
}

async fn two(inflight_a: usize) -> Two {
    let (a, b) = (Quiet::default(), Quiet::default());
    let ta = team(
        "a",
        a.clone(),
        r#"{"agents":{"tok-a-w1":"w1"},"people":{"tok-a-gab":"gab","tok-both":"kim"},"ingest":{"tok-a-ing":"ing"}}"#,
        inflight_a,
    )
    .await;
    let tb = team(
        "b",
        b.clone(),
        r#"{"agents":{"tok-b-w1":"w1"},"people":{"tok-b-ana":"ana","tok-both":"kim"}}"#,
        64,
    )
    .await;
    let teams = Arc::new(Teams::new(vec![ta, tb]).unwrap());
    Two {
        app: router(teams.clone()),
        teams,
        a,
        b,
    }
}

fn imported(item: &str) -> Value {
    json!({ "record": { "kind": "imported", "item": item, "source": "gh", "upstream_id": item, "title": "t" } })
}

async fn call(app: &Router, path: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
    send(app, Request::post(path), token, body).await
}

async fn get_json(app: &Router, path: &str, token: Option<&str>) -> (StatusCode, Value) {
    send(app, Request::get(path), token, Value::Null).await
}

async fn send(
    app: &Router,
    req: axum::http::request::Builder,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = req.header("content-type", "application/json");
    if let Some(t) = token {
        req = req.header("authorization", format!("Bearer {t}"));
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn a_call_without_a_known_token_is_refused() {
    let app = app().await;
    let body = json!({ "wait_seconds": 0 });
    assert_eq!(
        call(&app, "/v1/work/next", None, body.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "/v1/work/next", Some("nope"), body).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn an_agent_token_claims_and_a_person_token_does_not() {
    let app = app().await;
    let body = json!({ "wait_seconds": 0 });
    let (status, reply) = call(&app, "/v1/work/next", Some("tok-w1"), body.clone()).await;
    assert_eq!(
        (status, reply["claimed"].as_bool()),
        (StatusCode::OK, Some(false))
    );
    assert_eq!(
        call(&app, "/v1/work/next", Some("tok-gab"), body).await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn filters_and_the_peek_are_for_an_agent_token_and_bad_filters_are_refused() {
    let app = app().await;
    let filtered =
        json!({ "wait_seconds": 0, "kinds": ["fix"], "tiers": [2], "exclude_roots": ["x"] });
    // The test log is below format 15, which cannot serve a filtered claim.
    let (status, _) = call(&app, "/v1/work/next", Some("tok-w1"), filtered).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, reply) = call(&app, "/v1/work/peek", Some("tok-w1"), json!({ "limit": 3 })).await;
    assert_eq!(
        (status, reply["next"].as_array().map(Vec::len)),
        (StatusCode::OK, Some(0))
    );
    assert_eq!(
        call(&app, "/v1/work/peek", Some("tok-gab"), json!({}))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "/v1/work/peek", None, json!({})).await.0,
        StatusCode::UNAUTHORIZED
    );
    for bad in [json!({ "kinds": ["nonsense"] }), json!({ "tiers": [9] })] {
        for path in ["/v1/work/next", "/v1/work/peek"] {
            let (status, _) = call(&app, path, Some("tok-w1"), bad.clone()).await;
            assert!(status.is_client_error(), "{path} {bad}: {status}");
        }
    }
}

#[tokio::test]
async fn only_a_person_token_appends_records() {
    let app = app().await;
    let record = json!({ "record": { "kind": "imported", "item": "a1", "source": "gh", "upstream_id": "1", "title": "t" } });
    assert_eq!(
        call(&app, "/v1/work/records", Some("tok-w1"), record.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, reply) = call(&app, "/v1/work/records", Some("tok-gab"), record).await;
    assert_eq!(
        (status, reply["kind"].as_str()),
        (StatusCode::OK, Some("imported"))
    );
}

#[tokio::test]
async fn a_persons_note_is_a_human_input() {
    let app = app().await;
    let imported = json!({ "record": { "kind": "imported", "item": "a1", "source": "gh", "upstream_id": "1", "title": "t" } });
    call(&app, "/v1/work/records", Some("tok-gab"), imported).await;
    let note = json!({ "item": "a1", "input": "review_comment", "text": { "text": "why not the client?" } });
    let (status, reply) = call(&app, "/v1/work/note", Some("tok-gab"), note).await;
    assert_eq!(
        (status, reply["kind"].as_str()),
        (StatusCode::OK, Some("human_input"))
    );
    // A note on an item that does not exist is refused by the fold.
    let missing = json!({ "item": "zz", "text": { "text": "x" } });
    let (status, _) = call(&app, "/v1/work/note", Some("tok-gab"), missing).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_token_pasted_in_a_note_is_cut_before_the_write() {
    let felix = Quiet::default();
    felix.keeps.store(true, Ordering::SeqCst);
    let tokens = r#"{"agents":{},"people":{"tok-gab":"gab"},"ingest":{}}"#;
    let one = team("default", felix.clone(), tokens, 64).await;
    let app = router(Arc::new(Teams::new(vec![one]).unwrap()));
    call(&app, "/v1/work/records", Some("tok-gab"), imported("a1")).await;
    // Built at run time so no secret scanner mistakes this file for a leak.
    let secret = format!("ghp_{}", "n".repeat(36));
    let note = json!({ "item": "a1", "text": { "text": format!("use {secret}") } });
    let (status, _) = call(&app, "/v1/work/note", Some("tok-gab"), note).await;
    assert_eq!(status, StatusCode::OK);
    let log: Vec<String> = felix
        .log
        .lock()
        .unwrap()
        .iter()
        .map(|(_, p)| String::from_utf8_lossy(p).into_owned())
        .collect();
    assert!(log.iter().any(|p| p.contains("use [redacted]")), "{log:?}");
    assert!(!log.iter().any(|p| p.contains(&secret)));
}

fn notes_logged(felix: &Quiet) -> usize {
    felix
        .log
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, p)| String::from_utf8_lossy(p).contains("\"kind\":\"human_input\""))
        .count()
}

#[tokio::test]
async fn a_request_key_makes_a_retried_write_one_record() {
    let felix = Quiet::default();
    felix.keeps.store(true, Ordering::SeqCst);
    let tokens = r#"{"agents":{"tok-w1":"w1"},"people":{"tok-gab":"gab"},"ingest":{}}"#;
    let one = team("default", felix.clone(), tokens, 64).await;
    let app = router(Arc::new(Teams::new(vec![one]).unwrap()));
    call(&app, "/v1/work/records", Some("tok-gab"), imported("a1")).await;
    let note = |text: &str| json!({ "item": "a1", "text": { "text": text }, "request_key": "n-1" });

    let (status, first) = call(&app, "/v1/work/note", Some("tok-gab"), note("hi")).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    let (status, again) = call(&app, "/v1/work/note", Some("tok-gab"), note("hi")).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        (again["replayed"].as_bool(), &again["offset"]),
        (Some(true), &first["offset"])
    );
    assert_eq!(notes_logged(&felix), 1);

    let (status, other) = call(&app, "/v1/work/note", Some("tok-gab"), note("changed")).await;
    assert_eq!(
        (status, other["why"].as_str()),
        (StatusCode::CONFLICT, Some("request_key_reused"))
    );
    // The same key on another route is another request.
    let tier = json!({ "item": "a1", "tier": 1, "request_key": "n-1" });
    let (status, reply) = call(&app, "/v1/work/tier", Some("tok-gab"), tier).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("request_key_reused"))
    );
    assert_eq!(notes_logged(&felix), 1);

    // A key that is not a key, and a prerequisite, which a claim replays.
    for bad in [json!("has space"), json!(7), json!("")] {
        let body = json!({ "item": "a1", "text": { "text": "x" }, "request_key": bad });
        let (status, _) = call(&app, "/v1/work/note", Some("tok-gab"), body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let prereq = json!({ "item": "a1", "key": "k", "work": "prerequisite", "request_key": "p-1" });
    let (status, _) = call(&app, "/v1/work/post", Some("tok-w1"), prereq).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // An agent's keyed write is refused as it always was when it holds nothing.
    let progress = json!({ "item": "a1", "note": { "text": "x" }, "request_key": "g-1" });
    let (status, reply) = call(&app, "/v1/work/progress", Some("tok-w1"), progress).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("not_held"))
    );
}

/// A team whose Felix keeps what is published, so a reread finds it.
async fn kept() -> Router {
    let felix = Quiet::default();
    felix.keeps.store(true, Ordering::SeqCst);
    let tokens = r#"{"agents":{"tok-w1":"w1"},"people":{"tok-gab":"gab"},"ingest":{}}"#;
    let one = team("default", felix, tokens, 64).await;
    router(Arc::new(Teams::new(vec![one]).unwrap()))
}

async fn raise_to(app: &Router, to: u32) {
    let raise = json!({ "record": { "kind": "format_raised", "to": to } });
    let (status, reply) = call(app, "/v1/work/records", Some("tok-gab"), raise).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
}

#[tokio::test]
async fn a_person_reads_items_one_item_and_its_history() {
    let app = kept().await;
    for item in ["a1", "a2"] {
        call(&app, "/v1/work/records", Some("tok-gab"), imported(item)).await;
    }
    call(
        &app,
        "/v1/work/proposals/accept",
        Some("tok-gab"),
        json!({ "item": "a1", "tier": 2 }),
    )
    .await;
    call(
        &app,
        "/v1/work/note",
        Some("tok-gab"),
        json!({ "item": "a1", "text": { "text": "look at the cache" } }),
    )
    .await;

    let (status, all) = get_json(&app, "/v1/work/items", Some("tok-gab")).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    let ids: Vec<&str> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["item"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["a1", "a2"]);
    assert_eq!(all["items"][0]["stage"], "ready");
    assert_eq!(all["items"][0]["tier"], 2);

    let (_, triage) = get_json(&app, "/v1/work/items?stage=triage", Some("tok-gab")).await;
    assert_eq!(triage["items"].as_array().unwrap().len(), 1);
    assert_eq!(triage["items"][0]["item"], "a2");
    let (status, _) = get_json(&app, "/v1/work/items?stage=nope", Some("tok-gab")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, one) = get_json(&app, "/v1/work/items/a1", Some("tok-gab")).await;
    assert_eq!(status, StatusCode::OK, "{one}");
    assert_eq!(
        (one["item"]["id"].as_str(), one["stage"].as_str()),
        (Some("a1"), Some("ready"))
    );

    // A record about another item that points at a1 is in a1's history too.
    let link = json!({ "record": { "kind": "linked", "item": "a2", "to": "a1" } });
    let (status, reply) = call(&app, "/v1/work/records", Some("tok-gab"), link).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let (status, h) = get_json(&app, "/v1/work/items/a1/history", Some("tok-gab")).await;
    assert_eq!(status, StatusCode::OK, "{h}");
    let kinds: Vec<&str> = h["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["record"]["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["imported", "accepted", "human_input", "linked"]);
    assert_eq!(h["records"][2]["record"]["by"], "gab");
    assert_eq!(h["next"], Value::Null);
    // From an offset on, the older records are left out.
    let from = h["records"][1]["offset"].as_u64().unwrap();
    let (_, later) = get_json(
        &app,
        &format!("/v1/work/items/a1/history?from={from}"),
        Some("tok-gab"),
    )
    .await;
    assert_eq!(later["records"].as_array().unwrap().len(), 3);

    // The team form serves the same answers: the outer route's params must
    // not reach the inner route's `Path`.
    for (short, long) in [
        ("/v1/work/items/a1", "/v1/teams/default/work/items/a1"),
        (
            "/v1/work/items/a1/history",
            "/v1/teams/default/work/items/a1/history",
        ),
    ] {
        let (status, in_team) = get_json(&app, long, Some("tok-gab")).await;
        assert_eq!(status, StatusCode::OK, "{long}: {in_team}");
        assert_eq!(in_team, get_json(&app, short, Some("tok-gab")).await.1);
    }

    let (status, missing) = get_json(&app, "/v1/work/items/zz/history", Some("tok-gab")).await;
    assert_eq!(
        (status, missing["why"].as_str()),
        (StatusCode::CONFLICT, Some("unknown_item"))
    );
    for path in [
        "/v1/work/items",
        "/v1/work/items/a1",
        "/v1/work/items/a1/history",
    ] {
        assert_eq!(
            get_json(&app, path, Some("tok-w1")).await.0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
        assert_eq!(
            get_json(&app, path, None).await.0,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_person_moves_an_item_into_needs_you_and_out_once_the_log_is_at_format_7() {
    let app = kept().await;
    call(&app, "/v1/work/records", Some("tok-gab"), imported("a1")).await;
    call(
        &app,
        "/v1/work/proposals/accept",
        Some("tok-gab"),
        json!({ "item": "a1" }),
    )
    .await;
    let flag = json!({ "item": "a1" });

    // Format 7 is not raised yet: nothing moves.
    let (status, reply) = call(&app, "/v1/work/flag", Some("tok-gab"), flag.clone()).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("format"))
    );

    raise_to(&app, 7).await;
    let (status, reply) = call(&app, "/v1/work/flag", Some("tok-gab"), flag.clone()).await;
    assert_eq!(
        (status, reply["kind"].as_str()),
        (StatusCode::OK, Some("flagged")),
        "{reply}"
    );
    let (_, one) = get_json(&app, "/v1/work/items/a1", Some("tok-gab")).await;
    assert_eq!(
        (one["stage"].as_str(), one["needs"].as_str()),
        (Some("needs_you"), Some("flagged"))
    );

    let (status, reply) = call(&app, "/v1/work/flag", Some("tok-gab"), flag.clone()).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("no_change"))
    );
    let (status, _) = call(&app, "/v1/work/unflag", Some("tok-gab"), flag.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (_, one) = get_json(&app, "/v1/work/items/a1", Some("tok-gab")).await;
    assert_eq!(one["stage"], "ready");
    let (status, reply) = call(&app, "/v1/work/unflag", Some("tok-gab"), flag).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("no_change"))
    );
}

#[tokio::test]
async fn a_person_blocks_an_item_on_a_resource_and_lifts_it_once_the_log_is_at_format_16() {
    let app = kept().await;
    call(&app, "/v1/work/records", Some("tok-gab"), imported("a1")).await;
    call(
        &app,
        "/v1/work/proposals/accept",
        Some("tok-gab"),
        json!({ "item": "a1" }),
    )
    .await;
    let block = json!({ "item": "a1", "resource": "azure-vms", "reason": "no credit" });

    let (status, reply) = call(&app, "/v1/work/block", Some("tok-gab"), block.clone()).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("format"))
    );
    raise_to(&app, 16).await;
    let (status, reply) = call(&app, "/v1/work/block", Some("tok-gab"), block.clone()).await;
    assert_eq!(
        (status, reply["kind"].as_str()),
        (StatusCode::OK, Some("block_set")),
        "{reply}"
    );
    let (_, one) = get_json(&app, "/v1/work/items/a1", Some("tok-gab")).await;
    assert_eq!(one["item"]["block"]["resource"], "azure-vms", "{one}");
    // Neither a time nor a resource is a bad block, and an agent cannot set one.
    let none = json!({ "item": "a1", "reason": "x" });
    let (status, reply) = call(&app, "/v1/work/block", Some("tok-gab"), none).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("bad_block"))
    );
    let (status, _) = call(&app, "/v1/work/block", Some("tok-w1"), block).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let lift = json!({ "item": "a1" });
    let (status, _) = call(&app, "/v1/work/lift-block", Some("tok-gab"), lift.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let (status, reply) = call(&app, "/v1/work/lift-block", Some("tok-gab"), lift).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("no_change"))
    );
}

#[tokio::test]
async fn a_lost_claim_is_a_conflict_with_a_reason() {
    let app = app().await;
    let (status, reply) = call(
        &app,
        "/v1/work/extend",
        Some("tok-w1"),
        json!({ "item": "a1" }),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("lost"))
    );
}

#[tokio::test]
async fn the_ingest_token_writes_sync_records_and_nothing_else() {
    let app = app().await;
    let record = json!({ "record": { "kind": "imported", "item": "a1", "source": "gh", "upstream_id": "1", "title": "t" } });
    for token in ["tok-w1", "tok-gab"] {
        assert_eq!(
            call(&app, "/v1/ingest/records", Some(token), record.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let (status, reply) = call(&app, "/v1/ingest/records", Some("tok-ing"), record).await;
    assert_eq!(
        (status, reply["kind"].as_str()),
        (StatusCode::OK, Some("imported"))
    );
    let claim = json!({ "wait_seconds": 0 });
    assert_eq!(
        call(&app, "/v1/work/next", Some("tok-ing"), claim).await.0,
        StatusCode::FORBIDDEN
    );
    let budget = json!({ "record": { "kind": "budget_set", "root": "a1", "items_left": 9, "max_depth": 3, "cost_left_micros": 1, "redos_per_item": 3 } });
    let (status, reply) = call(&app, "/v1/ingest/records", Some("tok-ing"), budget).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("not_allowed"))
    );
}

#[tokio::test]
async fn an_ingest_token_tied_to_a_source_reaches_only_that_sources_roots() {
    let tokens = r#"{"agents":{},"people":{},"ingest":{"tok-gh":"ingest-gh","tok-jira":"ingest-jira"},"ingest_sources":{"ingest-gh":"github","ingest-jira":"jira"}}"#;
    let one = team("default", Quiet::default(), tokens, 64).await;
    let app = router(Arc::new(Teams::new(vec![one]).unwrap()));
    let import = |item: &str, source: &str| json!({ "record": { "kind": "imported", "item": item, "source": source, "upstream_id": item, "title": "t" } });
    let why = |(status, reply): (StatusCode, serde_json::Value)| {
        (status, reply["why"].as_str().map(str::to_owned))
    };
    let post = |token: &'static str, body: serde_json::Value| {
        let app = &app;
        async move { call(app, "/v1/ingest/records", Some(token), body).await }
    };
    assert_eq!(
        post("tok-jira", import("j1", "jira")).await.0,
        StatusCode::OK
    );
    assert_eq!(
        post("tok-gh", import("g1", "github")).await.0,
        StatusCode::OK
    );
    // A worker imports only its own source's issues.
    assert_eq!(
        why(post("tok-gh", import("j2", "jira")).await),
        (StatusCode::CONFLICT, Some("wrong_source".into()))
    );
    // The GitHub token cannot write to a Jira root, except the state of a request.
    let context = json!({ "record": { "kind": "context", "item": "j1", "entry": "issue", "text": { "text": "x" } } });
    assert_eq!(
        why(post("tok-gh", context.clone()).await),
        (StatusCode::CONFLICT, Some("not_imported".into()))
    );
    let state = |item: &str| {
        json!({ "record": { "kind": "pr_status", "item": item, "status": {
            "pr": "GetFelix/felix#1", "phase": "open", "review": "unreviewed", "checks": "green" } } })
    };
    assert_eq!(
        why(post("tok-gh", state("j1")).await),
        (StatusCode::CONFLICT, Some("wrong_pr".into()))
    );
    // A tied token keeps only its own source's cursor.
    for (token, own, other) in [("tok-gh", "github", "jira"), ("tok-jira", "jira", "github")] {
        let get = |source: &str| {
            let app = &app;
            let path = format!("/v1/ingest/state/{source}");
            async move { get_json(app, &path, Some(token)).await.0 }
        };
        assert_ne!(get(own).await, StatusCode::FORBIDDEN);
        assert_eq!(get(other).await, StatusCode::FORBIDDEN);
    }
    // The Jira token reaches its own root and not another source's request state.
    assert_eq!(post("tok-jira", context).await.0, StatusCode::OK);
    assert_eq!(
        why(post("tok-jira", state("g1")).await),
        (StatusCode::CONFLICT, Some("not_imported".into()))
    );
}

#[tokio::test]
async fn the_work_tools_are_for_agents_and_the_dashboard_routes_for_people() {
    let app = app().await;
    let agent_routes = [
        (
            "/v1/work/post",
            json!({ "item": "a1", "key": "k", "work": "split", "title": "t" }),
        ),
        ("/v1/work/link", json!({ "item": "a1", "to": "a2" })),
        ("/v1/work/context", json!({ "item": "a1" })),
        (
            "/v1/work/context/add",
            json!({ "item": "a1", "entry": "e", "text": { "text": "x" } }),
        ),
        (
            "/v1/work/progress",
            json!({ "item": "a1", "note": { "text": "x" } }),
        ),
        (
            "/v1/work/triage",
            json!({ "item": "a1", "tier": 1, "why": "w" }),
        ),
        (
            "/v1/work/step",
            json!({ "item": "a1", "command": "c", "exit_code": 0, "output": { "text": "x" } }),
        ),
        (
            "/v1/work/edit",
            json!({ "item": "a1", "file": "f", "hunk": { "text": "x" }, "decision": 1 }),
        ),
        (
            "/v1/work/ask",
            json!({ "item": "a1", "text": { "text": "x" } }),
        ),
    ];
    for (path, body) in agent_routes {
        assert_eq!(
            call(&app, path, Some("tok-gab"), body.clone()).await.0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
        assert_eq!(
            call(&app, path, None, body).await.0,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
    let person_routes = [
        ("/v1/work/proposals/accept", json!({ "item": "a1" })),
        (
            "/v1/work/proposals/edit",
            json!({ "item": "a1", "tier": 1 }),
        ),
        ("/v1/work/proposals/reject", json!({ "item": "a1" })),
        (
            "/v1/work/questions/answer",
            json!({ "item": "a1", "text": { "text": "x" } }),
        ),
        ("/v1/work/tier", json!({ "item": "a1", "tier": 1 })),
        (
            "/v1/work/note",
            json!({ "item": "a1", "text": { "text": "x" } }),
        ),
        ("/v1/work/close", json!({ "item": "a1" })),
        ("/v1/work/unlink", json!({ "item": "a1", "to": "a2" })),
        ("/v1/work/relate", json!({ "item": "a1", "to": "a2" })),
        ("/v1/work/unrelate", json!({ "item": "a1", "to": "a2" })),
        ("/v1/work/flag", json!({ "item": "a1" })),
        ("/v1/work/unflag", json!({ "item": "a1" })),
        ("/v1/work/retry", json!({ "item": "a1" })),
        ("/v1/work/snooze", json!({ "item": "a1", "until_ms": 1 })),
        (
            "/v1/work/block",
            json!({ "item": "a1", "resource": "azure-vms", "reason": "x" }),
        ),
        ("/v1/work/lift-block", json!({ "item": "a1" })),
        ("/v1/work/park", json!({ "item": "a1" })),
        ("/v1/work/unpark", json!({ "item": "a1" })),
        ("/v1/work/unblock", json!({ "item": "a1", "to": "a2" })),
        (
            "/v1/work/request-review",
            json!({ "item": "a1", "reviewer": "kim" }),
        ),
        ("/v1/work/request-changes", json!({ "item": "a1" })),
        ("/v1/work/mark-merged", json!({ "item": "a1" })),
        (
            "/v1/work/rate-summary",
            json!({ "item": "a1", "attempt": 1, "accurate": true }),
        ),
        ("/v1/work/take", json!({ "item": "a1" })),
        (
            "/v1/work/reassign",
            json!({ "item": "a1", "to": { "person": "gab" } }),
        ),
        (
            "/v1/work/steer",
            json!({ "item": "a1", "attempt": 1, "text": { "text": "x" } }),
        ),
        (
            "/v1/work/budget",
            json!({ "root": "a1", "items_left": 1, "max_depth": 1, "cost_left_micros": 1, "redos_per_item": 1 }),
        ),
    ];
    for (path, body) in person_routes {
        assert_eq!(
            call(&app, path, Some("tok-w1"), body.clone()).await.0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
        assert_eq!(
            call(&app, path, None, body).await.0,
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_decision_without_a_phase_or_a_commit_never_reaches_the_tracker() {
    let app = app().await;
    let full = json!({
        "item": "a1", "phase": "cause", "so_far": "s", "why": "w", "commit": "c",
        "evidence": [{ "stream": "work.events", "offset": 0 }]
    });
    // No phase: the body does not parse.
    let mut no_phase = full.clone();
    no_phase.as_object_mut().unwrap().remove("phase");
    assert_eq!(
        call(&app, "/v1/work/decide", Some("tok-w1"), no_phase)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut no_commit = full.clone();
    no_commit.as_object_mut().unwrap().remove("commit");
    assert_eq!(
        call(&app, "/v1/work/decide", Some("tok-w1"), no_commit)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut bad_phase = full.clone();
    bad_phase["phase"] = json!("guess");
    assert_eq!(
        call(&app, "/v1/work/decide", Some("tok-w1"), bad_phase)
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // With every field it parses, and then the tracker says the agent holds nothing.
    let (status, reply) = call(&app, "/v1/work/decide", Some("tok-w1"), full).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("not_held"))
    );
}

#[tokio::test]
async fn an_unknown_kind_of_work_is_a_bad_request() {
    let app = app().await;
    let (status, reply) = call(
        &app,
        "/v1/work/post",
        Some("tok-w1"),
        json!({ "item": "a1", "key": "k", "work": "triage", "title": "t" }),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid"))
    );
}

#[tokio::test]
async fn a_prerequisite_is_an_agent_call_with_one_of_two_shapes() {
    let app = app().await;
    let new = json!({
        "item": "a1", "key": "k", "work": "prerequisite", "title": "t",
        "target_source": "github", "target_repo": "o/r", "why": { "text": "w" }
    });
    // People post nothing on an agent's behalf.
    assert_eq!(
        call(&app, "/v1/work/post", Some("tok-gab"), new.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    // Nor can anyone write the record through the person or ingest routes.
    let record = json!({ "record": {
        "kind": "prerequisite", "item": "a1", "attempt": 1, "prerequisite": "g1", "key": "k"
    } });
    for (path, token) in [
        ("/v1/work/records", "tok-gab"),
        ("/v1/ingest/records", "tok-ing"),
    ] {
        let (status, _) = call(&app, path, Some(token), record.clone()).await;
        assert_ne!(status, StatusCode::OK);
    }
    // Mixing the shapes, or leaving out the reason, never reaches the tracker.
    for bad in [
        json!({ "item": "a1", "key": "k", "work": "prerequisite", "title": "t", "existing": "g1" }),
        json!({ "item": "a1", "key": "k", "work": "prerequisite", "title": "t", "target_source": "github", "target_repo": "o/r" }),
    ] {
        let (status, reply) = call(&app, "/v1/work/post", Some("tok-w1"), bad).await;
        assert_eq!(
            (status, reply["why"].as_str()),
            (StatusCode::BAD_REQUEST, Some("invalid"))
        );
    }
    // A well-formed one reaches the holder check.
    let (status, reply) = call(&app, "/v1/work/post", Some("tok-w1"), new).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("not_held"))
    );
}

#[tokio::test]
async fn an_agent_token_cannot_move_work_to_another_repo() {
    let app = app().await;
    let body = json!({
        "item": "a1", "question": 1, "text": { "text": "x" },
        "moves_to": { "source": "gh", "repo": "o/other", "title": "t" }
    });
    let (status, _) = call(&app, "/v1/work/answer", Some("tok-w1"), body).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn only_a_person_token_sets_rules() {
    let app = app().await;
    let body = json!({ "source": "github", "repo": "GetFelix/felix", "text": "one worktree", "checks": ["task lint"] });
    for token in ["tok-w1", "tok-ing"] {
        assert_eq!(
            call(&app, "/v1/work/rules", Some(token), body.clone())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    let (status, reply) = call(&app, "/v1/work/rules", Some("tok-gab"), body).await;
    assert_eq!(
        (status, reply["kind"].as_str()),
        (StatusCode::OK, Some("rules_set"))
    );
}

#[tokio::test]
async fn a_token_acts_only_in_its_own_team() {
    let two = two(64).await;
    let app = &two.app;
    // The token's own team, with and without the team in the path.
    for (path, item) in [
        ("/v1/work/records", "a1"),
        ("/v1/teams/a/work/records", "a2"),
    ] {
        assert_eq!(
            call(app, path, Some("tok-a-gab"), imported(item)).await.0,
            StatusCode::OK,
            "{path}"
        );
    }
    assert_eq!(two.a.published(), ["work.events", "work.events"]);
    // Another team's path answers as a team that does not exist, and writes nothing there.
    for (path, token) in [
        ("/v1/teams/b/work/records", "tok-a-gab"),
        ("/v1/teams/b/work/next", "tok-a-w1"),
        ("/v1/teams/b/ingest/records", "tok-a-ing"),
        ("/v1/teams/nope/work/records", "tok-a-gab"),
    ] {
        let (status, reply) = call(app, path, Some(token), imported("a1")).await;
        assert_eq!(
            (status, reply["why"].as_str()),
            (StatusCode::NOT_FOUND, Some("unknown_team")),
            "{path} with {token}"
        );
    }
    assert!(two.b.published().is_empty());
    assert_eq!(two.a.published().len(), 2);
    // A token is checked before the team.
    assert_eq!(
        call(
            app,
            "/v1/teams/b/work/records",
            Some("nope"),
            imported("a1")
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    // Only the work and ingest routes have a team form.
    assert_eq!(
        get_json(app, "/v1/teams/a/me", Some("tok-a-gab")).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_person_in_two_teams_names_the_team() {
    let two = two(64).await;
    let (status, reply) = call(
        &two.app,
        "/v1/work/records",
        Some("tok-both"),
        imported("a1"),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::BAD_REQUEST, Some("team_required"))
    );
    assert!(two.a.published().is_empty() && two.b.published().is_empty());
    let (status, _) = call(
        &two.app,
        "/v1/teams/b/work/records",
        Some("tok-both"),
        imported("a1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(two.b.published(), ["work.events"]);
    assert!(two.a.published().is_empty());
}

#[tokio::test]
async fn me_lists_the_persons_own_teams() {
    let two = two(64).await;
    let (status, reply) = get_json(&two.app, "/v1/me", Some("tok-b-ana")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["person"], "ana");
    assert_eq!(
        reply["teams"],
        json!([{ "team": "b", "role": "admin", "review_limit": 3 }])
    );
    let (_, reply) = get_json(&two.app, "/v1/me", Some("tok-both")).await;
    let names: Vec<&str> = reply["teams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["team"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a", "b"]);
    assert_eq!(
        get_json(&two.app, "/v1/me", Some("tok-a-w1")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get_json(&two.app, "/v1/me", None).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_busy_team_does_not_hold_up_another() {
    let two = two(1).await;
    let busy = two.teams.get("a").unwrap().admit(false).unwrap();
    let (status, reply) = call(
        &two.app,
        "/v1/work/records",
        Some("tok-a-gab"),
        imported("a1"),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("team_busy"))
    );
    assert!(two.a.published().is_empty());
    let (status, _) = call(
        &two.app,
        "/v1/work/records",
        Some("tok-b-ana"),
        imported("a1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    drop(busy);
    let (status, _) = call(
        &two.app,
        "/v1/work/records",
        Some("tok-a-gab"),
        imported("a1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn a_waiting_work_next_keeps_its_slot_and_not_the_others() {
    let two = two(1).await;
    let app = two.app.clone();
    let waiting = tokio::spawn(async move {
        call(
            &app,
            "/v1/work/next",
            Some("tok-a-w1"),
            json!({ "wait_seconds": 5 }),
        )
        .await
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (status, reply) = call(
        &two.app,
        "/v1/work/next",
        Some("tok-a-w1"),
        json!({ "wait_seconds": 0 }),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("team_busy"))
    );
    // A holder's calls are not behind the waiting poll.
    let (status, reply) = call(
        &two.app,
        "/v1/work/extend",
        Some("tok-a-w1"),
        json!({ "item": "a1" }),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("lost"))
    );
    assert_eq!(waiting.await.unwrap().0, StatusCode::OK);
}

#[tokio::test(start_paused = true)]
async fn a_team_that_loses_its_lease_answers_503_alone() {
    let two = two(64).await;
    let teams = two.teams.clone();
    let maintain = tokio::spawn(async move { teams.maintain().await });
    two.b.lease_lost.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(60)).await;
    let (status, _) = call(
        &two.app,
        "/v1/work/records",
        Some("tok-b-ana"),
        imported("a1"),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (status, _) = call(
        &two.app,
        "/v1/work/records",
        Some("tok-a-gab"),
        imported("a1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !maintain.is_finished(),
        "one team's lost lease stopped the tracker"
    );
    // With no team left holding its lease, the tracker stops.
    two.a.lease_lost.store(true, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(60), maintain)
        .await
        .expect("maintenance ends when every team lost its lease")
        .unwrap();
}

#[tokio::test]
async fn a_token_names_one_caller_across_teams() {
    let agent = r#"{"agents":{"tok-x":"w1"}}"#;
    let person = |name: &str| format!(r#"{{"people":{{"tok-x":"{name}"}}}}"#);
    for (a, b, ok) in [
        (agent.to_owned(), agent.to_owned(), false),
        (agent.to_owned(), person("kim"), false),
        (person("kim"), person("ana"), false),
        (person("kim"), person("kim"), true),
    ] {
        let teams = vec![
            team("a", Quiet::default(), &a, 64).await,
            team("b", Quiet::default(), &b, 64).await,
        ];
        assert_eq!(Teams::new(teams).is_ok(), ok, "{a} and {b}");
    }
    let twice = vec![
        team("a", Quiet::default(), "{}", 64).await,
        team("a", Quiet::default(), "{}", 64).await,
    ];
    assert!(Teams::new(twice).is_err());
}

#[tokio::test]
async fn a_reported_cost_is_refused_with_what_to_send_instead() {
    let app = app().await;
    let (status, reply) = call(
        &app,
        "/v1/work/complete",
        Some("tok-w1"),
        json!({ "item": "a1", "cost_micros": 5 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
    assert!(reply.to_string().contains("usage"), "{reply}");
    // An older kbl-mcp always sends 0: that gets past the check.
    let (status, reply) = call(
        &app,
        "/v1/work/complete",
        Some("tok-w1"),
        json!({ "item": "a1", "cost_micros": 0 }),
    )
    .await;
    assert_ne!(status, StatusCode::BAD_REQUEST, "{reply}");
}

#[tokio::test]
async fn only_a_person_sets_a_price_once_the_log_is_at_format_8() {
    let app = kept().await;
    let price = json!({ "model": "m", "unit": "input", "price_micros": 3_000_000 });
    assert_eq!(
        call(&app, "/v1/work/prices", Some("tok-w1"), price.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, reply) = call(&app, "/v1/work/prices", Some("tok-gab"), price.clone()).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("format"))
    );
    raise_to(&app, 8).await;
    let (status, reply) = call(&app, "/v1/work/prices", Some("tok-gab"), price).await;
    assert_eq!(
        (status, reply["kind"].as_str()),
        (StatusCode::OK, Some("price_set")),
        "{reply}"
    );
}

#[tokio::test]
async fn a_person_relates_two_items_from_format_18_and_the_panel_shows_both() {
    let app = kept().await;
    for item in ["a1", "a2"] {
        call(&app, "/v1/work/records", Some("tok-gab"), imported(item)).await;
    }
    let both = json!({ "item": "a1", "to": "a2" });
    let (status, r) = call(&app, "/v1/work/relate", Some("tok-gab"), both.clone()).await;
    assert_eq!(
        (status, r["why"].as_str()),
        (StatusCode::CONFLICT, Some("format")),
        "{r}"
    );
    raise_to(&app, 18).await;
    let (status, r) = call(&app, "/v1/work/relate", Some("tok-gab"), both.clone()).await;
    assert_eq!(
        (status, r["kind"].as_str()),
        (StatusCode::OK, Some("related")),
        "{r}"
    );
    for (id, other) in [("a1", "a2"), ("a2", "a1")] {
        let (_, panel) = get_json(&app, &format!("/v1/work/items/{id}"), Some("tok-gab")).await;
        assert_eq!(panel["related"], json!([other]), "{panel}");
        assert_eq!(panel["waits_on"], json!([]), "{panel}");
    }
    let (status, r) = call(&app, "/v1/work/unrelate", Some("tok-gab"), both).await;
    assert_eq!(
        (status, r["kind"].as_str()),
        (StatusCode::OK, Some("unrelated")),
        "{r}"
    );
    let (_, panel) = get_json(&app, "/v1/work/items/a1", Some("tok-gab")).await;
    assert_eq!(panel["related"], json!([]), "{panel}");
}

fn now_plus(ms: u64) -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + ms
}

/// A price that starts a minute from now, so the replacement's start names it.
async fn priced_for_later(app: &Router) -> u64 {
    raise_to(app, 8).await;
    let since = now_plus(60_000);
    let first = json!({ "model": "m", "unit": "input", "price_micros": 3_000_000, "effective_from_ms": since });
    let (status, reply) = call(app, "/v1/work/prices", Some("tok-gab"), first).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    since
}

fn replacing(price_micros: u64, expected: Value) -> Value {
    json!({
        "model": "m", "unit": "input", "price_micros": price_micros,
        "effective_from_ms": now_plus(120_000), "expected_price": expected,
    })
}

#[tokio::test]
async fn a_price_replaces_the_one_it_expected() {
    let app = kept().await;
    let since = priced_for_later(&app).await;
    let expected = json!({ "price_micros": 3_000_000, "since_ms": since });
    let (status, reply) = call(
        &app,
        "/v1/work/prices",
        Some("tok-gab"),
        replacing(4_000_000, expected),
    )
    .await;
    assert_eq!(
        (status, reply["kind"].as_str()),
        (StatusCode::OK, Some("price_set")),
        "{reply}"
    );
}

#[tokio::test]
async fn a_price_that_expected_another_is_refused_and_not_written() {
    let app = kept().await;
    let since = priced_for_later(&app).await;
    let tail = |reply: &Value| reply["offset"].as_u64();
    let stale = [
        json!({ "price_micros": 2_000_000, "since_ms": since }),
        json!({ "price_micros": 3_000_000, "since_ms": since + 1 }),
    ];
    for expected in stale {
        let (status, reply) = call(
            &app,
            "/v1/work/prices",
            Some("tok-gab"),
            replacing(4_000_000, expected),
        )
        .await;
        assert_eq!(
            (status, reply["why"].as_str()),
            (StatusCode::CONFLICT, Some("price_changed")),
            "{reply}"
        );
    }
    // Nothing was written: the next write lands at the offset right after the first price.
    let ok = replacing(
        5_000_000,
        json!({ "price_micros": 3_000_000, "since_ms": since }),
    );
    let (_, reply) = call(&app, "/v1/work/prices", Some("tok-gab"), ok).await;
    let (_, first) = call(
        &app,
        "/v1/work/prices",
        Some("tok-gab"),
        json!({ "model": "other", "unit": "input", "price_micros": 1 }),
    )
    .await;
    assert_eq!(tail(&first), tail(&reply).map(|o| o + 1), "{reply} {first}");
}

#[tokio::test]
async fn a_price_without_an_expected_price_is_not_checked() {
    let app = kept().await;
    priced_for_later(&app).await;
    let mut body = replacing(4_000_000, Value::Null);
    body.as_object_mut().unwrap().remove("expected_price");
    let (status, reply) = call(&app, "/v1/work/prices", Some("tok-gab"), body).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
}

#[tokio::test]
async fn of_two_prices_that_expect_the_same_one_only_one_lands() {
    let app = kept().await;
    let since = priced_for_later(&app).await;
    let expected = json!({ "price_micros": 3_000_000, "since_ms": since });
    let from = now_plus(120_000);
    let body = |p: u64| json!({ "model": "m", "unit": "input", "price_micros": p, "effective_from_ms": from, "expected_price": expected });
    let (a, b) = tokio::join!(
        call(&app, "/v1/work/prices", Some("tok-gab"), body(4_000_000)),
        call(&app, "/v1/work/prices", Some("tok-gab"), body(5_000_000)),
    );
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::CONFLICT],
        "{a:?} {b:?}"
    );
}

#[tokio::test]
async fn a_first_price_that_expected_none_lands_when_there_is_none() {
    let app = kept().await;
    raise_to(&app, 8).await;
    let body = json!({ "model": "m", "unit": "input", "price_micros": 1, "expected_price": { "none": true } });
    let (status, reply) = call(&app, "/v1/work/prices", Some("tok-gab"), body).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
}

#[tokio::test]
async fn a_price_that_expected_none_is_refused_when_one_is_in_effect() {
    let app = kept().await;
    let since = priced_for_later(&app).await;
    let (status, reply) = call(
        &app,
        "/v1/work/prices",
        Some("tok-gab"),
        replacing(4_000_000, json!({ "none": true })),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("price_changed")),
        "{reply}"
    );
    // Nothing was written: replacing the first price still works.
    let ok = replacing(
        5_000_000,
        json!({ "price_micros": 3_000_000, "since_ms": since }),
    );
    let (status, reply) = call(&app, "/v1/work/prices", Some("tok-gab"), ok).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
}

#[tokio::test]
async fn a_price_that_expected_a_price_is_refused_when_none_is_in_effect() {
    let app = kept().await;
    raise_to(&app, 8).await;
    let expected = json!({ "price_micros": 3_000_000, "since_ms": 1 });
    let (status, reply) = call(
        &app,
        "/v1/work/prices",
        Some("tok-gab"),
        replacing(4_000_000, expected),
    )
    .await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::CONFLICT, Some("price_changed")),
        "{reply}"
    );
}

#[tokio::test]
async fn of_two_first_prices_that_expect_none_only_one_lands() {
    let app = kept().await;
    raise_to(&app, 8).await;
    let body = |p: u64| json!({ "model": "m", "unit": "input", "price_micros": p, "expected_price": { "none": true } });
    let (a, b) = tokio::join!(
        call(&app, "/v1/work/prices", Some("tok-gab"), body(1)),
        call(&app, "/v1/work/prices", Some("tok-gab"), body(2)),
    );
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::CONFLICT],
        "{a:?} {b:?}"
    );
}

#[tokio::test]
async fn an_expected_price_that_is_not_a_price_or_none_true_is_a_bad_request() {
    let app = kept().await;
    raise_to(&app, 8).await;
    for expected in [
        json!({ "none": false }),
        json!({}),
        json!({ "none": true, "price_micros": 1 }),
        json!({ "price_micros": 1 }),
    ] {
        let body =
            json!({ "model": "m", "unit": "input", "price_micros": 1, "expected_price": expected });
        let (status, reply) = call(&app, "/v1/work/prices", Some("tok-gab"), body).await;
        assert!(
            status.is_client_error() && status != StatusCode::CONFLICT,
            "{expected}: {status} {reply}"
        );
    }
}

#[tokio::test]
async fn a_person_reads_time_to_human_action_and_an_agent_cannot() {
    let app = kept().await;
    let path = "/v1/work/metrics/time-to-human";
    let (status, m) = get_json(&app, path, Some("tok-gab")).await;
    assert_eq!(status, StatusCode::OK, "{m}");
    assert_eq!(m["days"], 30);
    assert_eq!(m["stats"]["count"], 0);
    assert_eq!(m["stats"]["open"], 0);
    assert!(m["stats"]["p50_ms"].is_null());
    assert_eq!(m["stats"]["by_kind"]["question"]["count"], 0);

    let (_, wide) = get_json(&app, &format!("{path}?days=99999"), Some("tok-gab")).await;
    assert_eq!(wide["days"], 366);
    let (_, one) = get_json(&app, &format!("{path}?days=0"), Some("tok-gab")).await;
    assert_eq!(one["days"], 1);

    let team = "/v1/teams/default/work/metrics/time-to-human";
    let (status, in_team) = get_json(&app, team, Some("tok-gab")).await;
    assert_eq!(status, StatusCode::OK, "{in_team}");
    assert_eq!(in_team["stats"]["by_kind"], m["stats"]["by_kind"]);

    assert_eq!(
        get_json(&app, path, Some("tok-w1")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(get_json(&app, path, None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_person_reads_summary_trust_and_an_agent_cannot() {
    let app = kept().await;
    let path = "/v1/work/metrics/summary-trust";
    let (status, m) = get_json(&app, path, Some("tok-gab")).await;
    assert_eq!(status, StatusCode::OK, "{m}");
    assert_eq!(m["days"], 30);
    assert_eq!(m["stats"]["rated"], 0);
    assert_eq!(m["stats"]["unrated"], 0);
    assert!(m["stats"]["accurate_permille"].is_null());

    let (_, wide) = get_json(&app, &format!("{path}?days=99999"), Some("tok-gab")).await;
    assert_eq!(wide["days"], 366);
    let (_, one) = get_json(&app, &format!("{path}?days=0"), Some("tok-gab")).await;
    assert_eq!(one["days"], 1);
    let team = "/v1/teams/default/work/metrics/summary-trust";
    let (status, in_team) = get_json(&app, team, Some("tok-gab")).await;
    assert_eq!(status, StatusCode::OK, "{in_team}");
    assert_eq!(in_team["stats"]["rated"], m["stats"]["rated"]);
    assert_eq!(in_team["days"], 30);

    assert_eq!(
        get_json(&app, path, Some("tok-w1")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(get_json(&app, path, None).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn my_queue_lists_what_waits_on_a_person_by_wait_then_tier() {
    let app = kept().await;
    raise_to(&app, 7).await;
    for item in ["a1", "a2", "a3"] {
        call(&app, "/v1/work/records", Some("tok-gab"), imported(item)).await;
        call(
            &app,
            "/v1/work/proposals/accept",
            Some("tok-gab"),
            json!({ "item": item, "tier": if item == "a2" { 1 } else { 2 } }),
        )
        .await;
    }
    // Accepted and unheld: nothing waits on a person yet.
    let (status, q) = get_json(&app, "/v1/work/queue", Some("tok-gab")).await;
    assert_eq!(status, StatusCode::OK, "{q}");
    assert_eq!(q["count"], 0);

    // The tier 2 item is flagged first, so it is older: age beats tier. The
    // pause keeps the two records in different milliseconds.
    for item in ["a1", "a2"] {
        tokio::time::sleep(Duration::from_millis(5)).await;
        let (status, r) = call(
            &app,
            "/v1/work/flag",
            Some("tok-gab"),
            json!({ "item": item }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{r}");
    }
    let (_, q) = get_json(&app, "/v1/work/queue", Some("tok-gab")).await;
    let ids: Vec<&str> = q["queue"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["item"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["a1", "a2"], "{q}");
    assert_eq!(q["queue"][0]["wait"], "other");
    assert_eq!(q["queue"][0]["needs"], "flagged");

    // The team form answers the same; an acted-on item leaves the queue.
    let team = get_json(&app, "/v1/teams/default/work/queue", Some("tok-gab")).await;
    assert_eq!(team.0, StatusCode::OK);
    assert_eq!(team.1["queue"], q["queue"]);
    call(
        &app,
        "/v1/work/unflag",
        Some("tok-gab"),
        json!({ "item": "a2" }),
    )
    .await;
    let (_, q) = get_json(&app, "/v1/work/queue", Some("tok-gab")).await;
    assert_eq!(q["count"], 1);
}

#[tokio::test]
async fn my_queue_limit_trims_the_list_but_not_the_count() {
    let app = kept().await;
    raise_to(&app, 7).await;
    for item in ["a1", "a2", "a3"] {
        call(&app, "/v1/work/records", Some("tok-gab"), imported(item)).await;
        call(
            &app,
            "/v1/work/proposals/accept",
            Some("tok-gab"),
            json!({ "item": item, "tier": 1 }),
        )
        .await;
        call(
            &app,
            "/v1/work/flag",
            Some("tok-gab"),
            json!({ "item": item }),
        )
        .await;
    }
    let (_, q) = get_json(&app, "/v1/work/queue?limit=2", Some("tok-gab")).await;
    assert_eq!(
        (q["queue"].as_array().unwrap().len(), &q["count"]),
        (2, &json!(3))
    );
    let (_, q) = get_json(&app, "/v1/work/queue", Some("tok-gab")).await;
    assert_eq!(q["queue"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn my_queue_is_for_a_person_and_stays_in_the_token_s_team() {
    let app = app().await;
    assert_eq!(
        get_json(&app, "/v1/work/queue", Some("tok-w1")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get_json(&app, "/v1/work/queue", Some("tok-ing")).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get_json(&app, "/v1/work/queue", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let two = two(64).await;
    let (status, reply) = get_json(&two.app, "/v1/teams/b/work/queue", Some("tok-a-gab")).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_team"))
    );
    let (status, reply) = get_json(&two.app, "/v1/work/queue", Some("tok-both")).await;
    assert_eq!(
        (status, reply["why"].as_str()),
        (StatusCode::BAD_REQUEST, Some("team_required"))
    );
}

fn now_ms() -> u64 {
    crate::tracker::now_ms()
}

/// A flagged item, so it waits on a person, in a log raised to format 10.
async fn waiting_item(app: &Router, prefix: &str, token: &str, item: &str) {
    let body = json!({ "item": item });
    let raise = json!({ "record": { "kind": "format_raised", "to": 10 } });
    call(app, &format!("{prefix}/work/records"), Some(token), raise).await;
    call(
        app,
        &format!("{prefix}/work/records"),
        Some(token),
        imported(item),
    )
    .await;
    call(
        app,
        &format!("{prefix}/work/proposals/accept"),
        Some(token),
        body.clone(),
    )
    .await;
    let (status, r) = call(app, &format!("{prefix}/work/flag"), Some(token), body).await;
    assert_eq!(status, StatusCode::OK, "{r}");
}

/// What `kbl-mcp person` sends: the card actions with a person's token. Each
/// record names that person, and an agent's token writes none of them.
#[tokio::test]
async fn a_person_tool_call_is_recorded_as_the_person() {
    let felix = Quiet::default();
    felix.keeps.store(true, Ordering::SeqCst);
    let tokens = r#"{"agents":{"tok-w1":"w1"},"people":{"tok-gab":"gab"},"ingest":{}}"#;
    let one = team("default", felix.clone(), tokens, 64).await;
    let app = router(Arc::new(Teams::new(vec![one]).unwrap()));
    raise_to(&app, 10).await;
    call(&app, "/v1/work/records", Some("tok-gab"), imported("a1")).await;
    let keyed = |k: &str| json!({ "item": "a1", "request_key": k });
    let calls = [
        (
            "/v1/work/proposals/accept",
            json!({ "item": "a1", "tier": 1 }),
        ),
        ("/v1/work/tier", json!({ "item": "a1", "tier": 2 })),
        (
            "/v1/work/note",
            json!({ "item": "a1", "text": { "text": "x" } }),
        ),
        ("/v1/work/flag", keyed("k-1")),
        ("/v1/work/unflag", keyed("k-2")),
    ];
    let before = felix.log.lock().unwrap().len();
    for (path, body) in &calls {
        let denied = call(&app, path, Some("tok-w1"), body.clone()).await;
        assert_eq!(denied.0, StatusCode::FORBIDDEN, "{path}");
    }
    assert_eq!(felix.log.lock().unwrap().len(), before, "an agent wrote");
    for (path, body) in calls {
        let (status, r) = call(&app, path, Some("tok-gab"), body).await;
        assert_eq!(status, StatusCode::OK, "{path}: {r}");
    }
    let log = felix.log.lock().unwrap().clone();
    let written: Vec<Value> = log[before..]
        .iter()
        .map(|(_, p)| serde_json::from_slice(p).unwrap())
        .collect();
    let kinds: Vec<&str> = written
        .iter()
        .map(|w| w["kind"].as_str().unwrap_or("?"))
        .collect();
    for want in ["accepted", "tier_changed", "human_input", "flagged"] {
        assert!(kinds.contains(&want), "{want} in {kinds:?}");
    }
    for w in written.iter().filter(|w| w["actor"].is_string()) {
        assert_eq!(
            (w["by"].as_str(), w["actor"].as_str()),
            (Some("gab"), Some("person")),
            "{w}"
        );
    }
}

#[tokio::test]
async fn the_card_actions_wait_for_format_10() {
    let app = kept().await;
    call(&app, "/v1/work/records", Some("tok-gab"), imported("a1")).await;
    let item = json!({ "item": "a1" });
    for (path, body) in [
        ("/v1/work/retry", item.clone()),
        (
            "/v1/work/snooze",
            json!({ "item": "a1", "until_ms": now_ms() + 60_000 }),
        ),
        ("/v1/work/park", item.clone()),
        ("/v1/work/unpark", item.clone()),
        ("/v1/work/unblock", json!({ "item": "a1", "to": "a2" })),
    ] {
        let (status, reply) = call(&app, path, Some("tok-gab"), body).await;
        assert_eq!(
            (status, reply["why"].as_str()),
            (StatusCode::CONFLICT, Some("format")),
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_parked_or_snoozed_item_leaves_my_queue_and_a_snooze_ends() {
    let app = kept().await;
    waiting_item(&app, "/v1", "tok-gab", "a1").await;
    let count = |app: &Router| {
        let app = app.clone();
        async move { get_json(&app, "/v1/work/queue", Some("tok-gab")).await.1["count"].clone() }
    };
    assert_eq!(count(&app).await, 1);

    let item = json!({ "item": "a1" });
    let (status, r) = call(&app, "/v1/work/park", Some("tok-gab"), item.clone()).await;
    assert_eq!(
        (status, r["kind"].as_str()),
        (StatusCode::OK, Some("parked")),
        "{r}"
    );
    assert_eq!(count(&app).await, 0);
    let (status, r) = call(&app, "/v1/work/park", Some("tok-gab"), item.clone()).await;
    assert_eq!(
        (status, r["why"].as_str()),
        (StatusCode::CONFLICT, Some("no_change"))
    );
    call(&app, "/v1/work/unpark", Some("tok-gab"), item.clone()).await;
    assert_eq!(count(&app).await, 1);

    let until = json!({ "item": "a1", "until_ms": now_ms() + 300 });
    let (status, r) = call(&app, "/v1/work/snooze", Some("tok-gab"), until).await;
    assert_eq!(
        (status, r["kind"].as_str()),
        (StatusCode::OK, Some("snoozed")),
        "{r}"
    );
    assert_eq!(count(&app).await, 0);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(count(&app).await, 1);

    let past = json!({ "item": "a1", "until_ms": 1 });
    let (status, r) = call(&app, "/v1/work/snooze", Some("tok-gab"), past).await;
    assert_eq!(
        (status, r["why"].as_str()),
        (StatusCode::CONFLICT, Some("bad_time"))
    );
    let nothing = json!({ "item": "a1", "to": "a2" });
    let (status, r) = call(&app, "/v1/work/unblock", Some("tok-gab"), nothing).await;
    assert_eq!(
        (status, r["why"].as_str()),
        (StatusCode::CONFLICT, Some("no_change"))
    );
    let (status, r) = call(&app, "/v1/work/retry", Some("tok-gab"), item).await;
    assert_eq!(
        (status, r["why"].as_str()),
        (StatusCode::CONFLICT, Some("no_change"))
    );
}

#[tokio::test]
async fn the_card_actions_act_only_in_the_tokens_team() {
    let two = two(64).await;
    waiting_item(&two.app, "/v1/teams/a", "tok-a-gab", "a1").await;
    let written = two.a.published().len();
    let item = json!({ "item": "a1" });
    for path in ["retry", "park", "unpark", "unblock"] {
        let (status, r) = call(
            &two.app,
            &format!("/v1/teams/b/work/{path}"),
            Some("tok-a-gab"),
            json!({ "item": "a1", "to": "a2" }),
        )
        .await;
        assert_eq!(
            (status, r["why"].as_str()),
            (StatusCode::NOT_FOUND, Some("unknown_team")),
            "{path}"
        );
    }
    let until = json!({ "item": "a1", "until_ms": now_ms() + 60_000 });
    let (status, r) = call(
        &two.app,
        "/v1/teams/b/work/snooze",
        Some("tok-a-gab"),
        until,
    )
    .await;
    assert_eq!(
        (status, r["why"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_team"))
    );
    assert!(two.b.published().is_empty());
    assert_eq!(two.a.published().len(), written);

    // Its own team parks it, and the other team's person cannot see or park it.
    let (status, r) = call(
        &two.app,
        "/v1/teams/a/work/park",
        Some("tok-a-gab"),
        item.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    let (_, q) = get_json(&two.app, "/v1/teams/a/work/queue", Some("tok-a-gab")).await;
    assert_eq!(q["count"], 0);
    let (status, _) = call(&two.app, "/v1/teams/b/work/park", Some("tok-b-ana"), item).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// The four format 11 routes, with a body each takes.
fn review_routes() -> [(&'static str, Value); 4] {
    [
        ("request-review", json!({ "item": "a1", "reviewer": "kim" })),
        ("request-changes", json!({ "item": "a1" })),
        ("mark-merged", json!({ "item": "a1" })),
        (
            "rate-summary",
            json!({ "item": "a1", "attempt": 1, "accurate": true }),
        ),
    ]
}

#[tokio::test]
async fn the_review_actions_wait_for_format_11() {
    let app = kept().await;
    call(&app, "/v1/work/records", Some("tok-gab"), imported("a1")).await;
    raise_to(&app, 10).await;
    for (path, body) in review_routes() {
        let (status, reply) = call(
            &app,
            &format!("/v1/work/{path}"),
            Some("tok-gab"),
            body.clone(),
        )
        .await;
        assert_eq!(
            (status, reply["why"].as_str()),
            (StatusCode::CONFLICT, Some("format")),
            "{path}"
        );
    }
    // Raised, the fold answers for each: there is no request and no attempt.
    raise_to(&app, 11).await;
    let want = [
        ("request-review", "no_change"),
        ("request-changes", "no_change"),
        ("mark-merged", "no_change"),
        ("rate-summary", "wrong_attempt"),
    ];
    for ((path, body), (_, why)) in review_routes().into_iter().zip(want) {
        let (status, reply) = call(&app, &format!("/v1/work/{path}"), Some("tok-gab"), body).await;
        assert_eq!(
            (status, reply["why"].as_str()),
            (StatusCode::CONFLICT, Some(why)),
            "{path}"
        );
    }
}

#[tokio::test]
async fn the_review_actions_act_only_in_the_tokens_team() {
    let two = two(64).await;
    let raise = json!({ "record": { "kind": "format_raised", "to": 11 } });
    call(
        &two.app,
        "/v1/teams/a/work/records",
        Some("tok-a-gab"),
        raise,
    )
    .await;
    call(
        &two.app,
        "/v1/teams/a/work/records",
        Some("tok-a-gab"),
        imported("a1"),
    )
    .await;
    let written = two.a.published().len();
    for (path, body) in review_routes() {
        // The other team is not this token's.
        let (status, r) = call(
            &two.app,
            &format!("/v1/teams/b/work/{path}"),
            Some("tok-a-gab"),
            body.clone(),
        )
        .await;
        assert_eq!(
            (status, r["why"].as_str()),
            (StatusCode::NOT_FOUND, Some("unknown_team")),
            "{path}"
        );
        // Its own team reads the fold; the item does not exist in team b.
        let (status, r) = call(
            &two.app,
            &format!("/v1/teams/b/work/{path}"),
            Some("tok-b-ana"),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{path}: {r}");
    }
    assert!(two.b.published().is_empty());
    assert_eq!(two.a.published().len(), written);
}

/// The three format 12 routes, with a body each takes.
fn assignee_routes() -> [(&'static str, Value); 3] {
    [
        ("take", json!({ "item": "a1" })),
        (
            "reassign",
            json!({ "item": "a1", "to": { "person": "gab" } }),
        ),
        (
            "steer",
            json!({ "item": "a1", "attempt": 1, "text": { "text": "x" } }),
        ),
    ]
}

#[tokio::test]
async fn the_assignee_actions_wait_for_format_12() {
    let app = kept().await;
    call(&app, "/v1/work/records", Some("tok-gab"), imported("a1")).await;
    raise_to(&app, 11).await;
    for (path, body) in assignee_routes() {
        let (status, reply) = call(
            &app,
            &format!("/v1/work/{path}"),
            Some("tok-gab"),
            body.clone(),
        )
        .await;
        assert_eq!(
            (status, reply["why"].as_str()),
            (StatusCode::CONFLICT, Some("format")),
            "{path}"
        );
    }
    // Raised, the fold answers for each: nobody accepted it, nobody holds it.
    raise_to(&app, 12).await;
    let want = [
        ("take", "not_ready"),
        ("reassign", "not_ready"),
        ("steer", "not_held"),
    ];
    for ((path, body), (_, why)) in assignee_routes().into_iter().zip(want) {
        let (status, reply) = call(&app, &format!("/v1/work/{path}"), Some("tok-gab"), body).await;
        assert_eq!(
            (status, reply["why"].as_str()),
            (StatusCode::CONFLICT, Some(why)),
            "{path}"
        );
    }
}

/// Team `a` raised to 12 with `a1` accepted.
async fn two_at_12() -> Two {
    let two = two(64).await;
    for record in [
        json!({ "record": { "kind": "format_raised", "to": 12 } }),
        imported("a1"),
        json!({ "record": { "kind": "accepted", "item": "a1", "tier": 1 } }),
    ] {
        let (status, r) = call(
            &two.app,
            "/v1/teams/a/work/records",
            Some("tok-a-gab"),
            record,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{r}");
    }
    two
}

#[tokio::test]
async fn the_assignee_actions_act_only_in_the_tokens_team() {
    let two = two_at_12().await;
    let written = two.a.published().len();
    for (path, body) in assignee_routes() {
        let (status, r) = call(
            &two.app,
            &format!("/v1/teams/b/work/{path}"),
            Some("tok-a-gab"),
            body.clone(),
        )
        .await;
        assert_eq!(
            (status, r["why"].as_str()),
            (StatusCode::NOT_FOUND, Some("unknown_team")),
            "{path}"
        );
    }
    assert!(two.b.published().is_empty());
    assert_eq!(two.a.published().len(), written);
}

#[tokio::test]
async fn a_reassign_names_a_person_of_the_team() {
    let two = two_at_12().await;
    let written = two.a.published().len();
    // ana is in team b only.
    let to_ana = json!({ "item": "a1", "to": { "person": "ana" } });
    let (status, r) = call(
        &two.app,
        "/v1/teams/a/work/reassign",
        Some("tok-a-gab"),
        to_ana,
    )
    .await;
    assert_eq!(
        (status, r["why"].as_str()),
        (StatusCode::CONFLICT, Some("not_member"))
    );
    assert_eq!(two.a.published().len(), written);
    // kim is in team a: assigned, and the item leaves gab's queue for kim's.
    let to_kim = json!({ "item": "a1", "to": { "person": "kim" } });
    let (status, r) = call(
        &two.app,
        "/v1/teams/a/work/reassign",
        Some("tok-a-gab"),
        to_kim,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    let queue = |token| {
        let app = two.app.clone();
        async move {
            let (status, q) = get_json(&app, "/v1/teams/a/work/queue", Some(token)).await;
            assert_eq!(status, StatusCode::OK, "{q}");
            q
        }
    };
    let kim = queue("tok-both").await;
    assert_eq!(kim["count"], 1);
    assert_eq!(kim["queue"][0]["assignee"], json!({ "person": "kim" }));
    assert_eq!(kim["queue"][0]["needs"], "taken");
    assert_eq!(queue("tok-a-gab").await["count"], 0);
    // The one-item read names the assignee too.
    let (_, item) = get_json(&two.app, "/v1/teams/a/work/items/a1", Some("tok-a-gab")).await;
    assert_eq!(item["item"]["assignee"], json!({ "person": "kim" }));
    // Nobody: it is Ready again and in no one's queue.
    let none = json!({ "item": "a1", "to": null });
    let (status, _) = call(
        &two.app,
        "/v1/teams/a/work/reassign",
        Some("tok-a-gab"),
        none,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queue("tok-both").await["count"], 0);
}

#[tokio::test]
async fn a_reassign_through_records_names_a_person_of_the_team_too() {
    let two = two_at_12().await;
    let written = two.a.published().len();
    let record = |who: &str| json!({ "record": { "kind": "reassigned", "item": "a1", "to": { "person": who } } });
    let (status, r) = call(
        &two.app,
        "/v1/teams/a/work/records",
        Some("tok-a-gab"),
        record("ana"),
    )
    .await;
    assert_eq!(
        (status, r["why"].as_str()),
        (StatusCode::CONFLICT, Some("not_member"))
    );
    assert_eq!(two.a.published().len(), written);
    let (status, r) = call(
        &two.app,
        "/v1/teams/a/work/records",
        Some("tok-a-gab"),
        record("kim"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
}

#[tokio::test]
async fn a_steer_over_the_inline_limit_is_refused_on_both_routes() {
    let two = two_at_12().await;
    let written = two.a.published().len();
    let big = "x".repeat(kbl_core::artifact::INLINE_LIMIT + 1);
    let body = json!({ "item": "a1", "attempt": 1, "text": { "text": big } });
    let record = json!({ "record": { "kind": "steered", "item": "a1", "attempt": 1, "text": { "text": big } } });
    for (path, body) in [("steer", body), ("records", record)] {
        let (status, r) = call(
            &two.app,
            &format!("/v1/teams/a/work/{path}"),
            Some("tok-a-gab"),
            body,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}: {r}");
    }
    assert_eq!(two.a.published().len(), written);
}
