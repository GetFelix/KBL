use std::collections::{HashMap, VecDeque};
use std::sync::Mutex as StdMutex;

use agents_core::names::RunId;
use bytes::Bytes;

use super::*;

/// A Felix that keeps everything in memory and lets a test script the polls.
#[derive(Clone, Default)]
struct Fake(Arc<StdMutex<State>>);

#[derive(Default)]
struct State {
    streams: HashMap<String, Vec<Vec<u8>>>,
    caches: HashMap<(String, String), Vec<u8>>,
    deliveries: VecDeque<GroupRecord>,
    calls: Vec<String>,
    polls: Vec<(String, GroupMember, Duration)>,
    fail: Option<&'static str>,
}

impl Fake {
    fn deliver(&self, payload: Vec<u8>, offset: u64, attempts: u32) {
        self.0.lock().unwrap().deliveries.push_back(GroupRecord {
            offset,
            payload: Bytes::from(payload),
            attempts,
            skipped_before: 0,
            publisher: None,
            timestamp_micros: None,
        });
    }

    fn fail_on(&self, op: &'static str) {
        self.0.lock().unwrap().fail = Some(op);
    }

    fn calls(&self, prefix: &str) -> Vec<String> {
        let state = self.0.lock().unwrap();
        state
            .calls
            .iter()
            .filter(|c| c.starts_with(prefix))
            .cloned()
            .collect()
    }

    fn cache(&self, cache: &str, key: &str) -> Option<Vec<u8>> {
        let state = self.0.lock().unwrap();
        state.caches.get(&(cache.into(), key.into())).cloned()
    }

    /// The records on the run log, in order.
    fn log(&self, run: &str) -> Vec<Record> {
        let state = self.0.lock().unwrap();
        state
            .streams
            .get(&format!("agents.log.{run}"))
            .into_iter()
            .flatten()
            .map(|bytes| Logged::decode(bytes).unwrap().record)
            .collect()
    }

    fn kinds(&self, run: &str) -> Vec<&'static str> {
        self.log(run).iter().map(Record::kind).collect()
    }

    fn step(&self, op: &'static str, what: String) -> anyhow::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.calls.push(format!("{op} {what}"));
        if state.fail == Some(op) {
            anyhow::bail!("{op}: permission denied");
        }
        Ok(())
    }
}

impl Backend for Fake {
    async fn publish(&self, stream: &str, payload: Vec<u8>) -> anyhow::Result<u64> {
        self.step("publish", stream.into())?;
        let mut state = self.0.lock().unwrap();
        let records = state.streams.entry(stream.into()).or_default();
        records.push(payload);
        Ok(records.len() as u64 - 1)
    }

    async fn cache_put(&self, cache: &str, key: &str, value: Vec<u8>) -> anyhow::Result<()> {
        self.step("cache_put", format!("{cache} {key}"))?;
        let mut state = self.0.lock().unwrap();
        state.caches.insert((cache.into(), key.into()), value);
        Ok(())
    }

    async fn cache_get(&self, cache: &str, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(self.cache(cache, key))
    }

    async fn cache_put_absent(
        &self,
        cache: &str,
        key: &str,
        value: Vec<u8>,
    ) -> anyhow::Result<bool> {
        self.step("cache_put_if", format!("{cache} {key}"))?;
        let mut state = self.0.lock().unwrap();
        let slot = state.caches.entry((cache.into(), key.into()));
        Ok(match slot {
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(value);
                true
            }
        })
    }

    async fn poll(
        &self,
        stream: &str,
        _group: &str,
        member: &GroupMember,
        visibility: Duration,
        _wait: Duration,
    ) -> anyhow::Result<Option<GroupRecord>> {
        self.step("poll", stream.into())?;
        let mut state = self.0.lock().unwrap();
        state
            .polls
            .push((stream.into(), member.clone(), visibility));
        Ok(state.deliveries.pop_front())
    }

    async fn ack(&self, _: &str, _: &str, offset: u64) -> anyhow::Result<()> {
        self.step("ack", offset.to_string())
    }

    async fn nack(&self, _: &str, _: &str, record: &GroupRecord) -> anyhow::Result<()> {
        self.step("nack", record.offset.to_string())
    }

    async fn dead_letter(&self, _: &str, _: &str, offset: u64) -> anyhow::Result<()> {
        self.step("dead_letter", offset.to_string())
    }

    async fn extend(
        &self,
        _: &str,
        _: &str,
        record: &GroupRecord,
        by: Duration,
    ) -> anyhow::Result<Duration> {
        self.step("extend", record.offset.to_string())?;
        Ok(by)
    }
}

const RUN: &str = "r1";

fn tasks(fake: &Fake) -> Tasks<Fake> {
    Tasks::new(
        fake.clone(),
        Resources::new(RunId::new(RUN).unwrap()),
        AgentName::new("w1").unwrap(),
    )
}

fn task_bytes(id: &str) -> Vec<u8> {
    Task {
        id: TaskId::new(id).unwrap(),
        parent: TaskId::new("root").unwrap(),
        title: "review a.rs".into(),
        body: Some(Content::Text("look for bugs".into())),
        lease: None,
        deadline_ms: None,
    }
    .encode()
}

fn json(reply: Result<String, String>) -> Value {
    serde_json::from_str(&reply.unwrap()).unwrap()
}

#[tokio::test]
async fn a_posted_task_lands_on_the_tasks_stream_and_the_log() {
    let fake = Fake::default();
    let reply = json(
        tasks(&fake)
            .post(Post {
                id: Some("t1".into()),
                title: "review a.rs".into(),
                body: Some("x".repeat(5000)),
                parent: None,
                lease: None,
                deadline_ms: None,
            })
            .await,
    );
    assert_eq!(
        (reply["task"].as_str(), reply["offset"].as_u64()),
        (Some("t1"), Some(0))
    );
    let sent = fake.0.lock().unwrap().streams["agents.tasks.r1"][0].clone();
    let task = Task::decode(&sent).unwrap();
    // A body over the inline limit is an artifact in the blobs cache.
    let Some(Content::Artifact(id)) = task.body else {
        panic!("a 5000 byte body should be an artifact")
    };
    assert_eq!(
        fake.cache("agents.blobs.r1", id.as_str()).unwrap().len(),
        5000
    );
    assert!(matches!(
        fake.log(RUN).as_slice(),
        [Record::TaskPosted { offset: 0, .. }]
    ));
}

#[tokio::test]
async fn a_claim_polls_as_the_named_member_with_a_90_second_visibility() {
    let fake = Fake::default();
    fake.deliver(task_bytes("t1"), 7, 1);
    let reply = json(tasks(&fake).claim(None).await);
    assert_eq!(reply["claimed"], true);
    assert_eq!(reply["task"]["body"], "look for bugs");
    let (stream, member, visibility) = fake.0.lock().unwrap().polls[0].clone();
    assert_eq!(stream, "agents.tasks.r1");
    assert_eq!((member.consumer.as_str(), member.reclaim), ("w1", true));
    assert_eq!(visibility, Duration::from_secs(90));
    assert!(matches!(
        fake.log(RUN).as_slice(),
        [Record::TaskClaimed {
            offset: 7,
            attempt: 1,
            ..
        }]
    ));
}

#[tokio::test]
async fn no_task_is_not_an_error() {
    let fake = Fake::default();
    assert_eq!(json(tasks(&fake).claim(None).await)["claimed"], false);
    assert!(fake.log(RUN).is_empty());
}

#[tokio::test]
async fn a_task_on_its_third_delivery_is_dead_lettered_not_handed_to_the_model() {
    let fake = Fake::default();
    fake.deliver(task_bytes("t1"), 3, 3);
    let t = tasks(&fake);
    assert_eq!(json(t.claim(None).await)["claimed"], false);
    assert_eq!(fake.calls("dead_letter"), ["dead_letter 3"]);
    assert_eq!(fake.kinds(RUN), ["task_dead"]);

    // The second delivery is still the worker's to try.
    fake.deliver(task_bytes("t2"), 4, 2);
    let reply = json(t.claim(None).await);
    assert_eq!(
        (reply["claimed"].as_bool(), reply["attempt"].as_u64()),
        (Some(true), Some(2))
    );
}

#[tokio::test]
async fn an_undecodable_record_is_dead_lettered_and_the_next_one_is_claimed() {
    let fake = Fake::default();
    fake.deliver(b"not json".to_vec(), 1, 1);
    fake.deliver(task_bytes("t2"), 2, 1);
    let reply = json(tasks(&fake).claim(None).await);
    assert_eq!(reply["task"]["id"], "t2");
    assert_eq!(fake.calls("dead_letter"), ["dead_letter 1"]);
}

#[tokio::test]
async fn one_task_is_held_at_a_time() {
    let fake = Fake::default();
    fake.deliver(task_bytes("t1"), 1, 1);
    let t = tasks(&fake);
    t.claim(None).await.unwrap();
    assert!(
        t.claim(None)
            .await
            .unwrap_err()
            .contains("already hold task t1")
    );
    assert!(
        t.complete("t9", "x".into())
            .await
            .unwrap_err()
            .contains("not t9")
    );
    assert!(
        tasks(&fake)
            .complete("t1", "x".into())
            .await
            .unwrap_err()
            .contains("do not hold")
    );
}

#[tokio::test]
async fn the_claim_is_extended_while_held_and_not_after() {
    let fake = Fake::default();
    fake.deliver(task_bytes("t1"), 5, 1);
    let t = Tasks::with_timing(
        fake.clone(),
        Resources::new(RunId::new(RUN).unwrap()),
        AgentName::new("w1").unwrap(),
        Duration::from_secs(90),
        Duration::from_millis(20),
    );
    t.claim(None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let during = fake.calls("extend").len();
    assert!(during >= 3, "extended {during} times");
    assert!(fake.kinds(RUN).contains(&"task_extended"));

    t.complete("t1", "done".into()).await.unwrap();
    let after = fake.calls("extend").len();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        fake.calls("extend").len(),
        after,
        "extended after completing"
    );
}

#[tokio::test]
async fn the_first_result_wins_and_a_second_copy_is_a_duplicate() {
    let fake = Fake::default();
    fake.deliver(task_bytes("t1"), 1, 1);
    let t = tasks(&fake);
    t.claim(None).await.unwrap();
    let first = json(t.complete("t1", "first".into()).await);
    assert_eq!(first["status"], "done");

    // Felix redelivers the same task, perhaps to another worker.
    fake.deliver(task_bytes("t1"), 1, 2);
    t.claim(None).await.unwrap();
    let second = json(t.complete("t1", "second".into()).await);
    assert_eq!(second["status"], "duplicate");

    let stored = TaskResult::decode(&fake.cache("agents.results.r1", "t1").unwrap()).unwrap();
    assert_eq!(stored.artifact, ArtifactId::of(b"first"));
    assert_eq!(stored.attempt, 1);
    // The losing result is kept.
    assert_eq!(
        fake.cache("agents.blobs.r1", ArtifactId::of(b"second").as_str())
            .unwrap(),
        b"second"
    );
    assert_eq!(fake.calls("ack"), ["ack 1", "ack 1"]);
    assert_eq!(
        fake.kinds(RUN),
        [
            "task_claimed",
            "task_done",
            "task_claimed",
            "task_duplicate"
        ]
    );
}

#[tokio::test]
async fn release_hands_the_task_back_and_give_up_dead_letters_it() {
    let fake = Fake::default();
    fake.deliver(task_bytes("t1"), 1, 1);
    fake.deliver(task_bytes("t2"), 2, 1);
    let t = tasks(&fake);
    t.claim(None).await.unwrap();
    t.release("t1", Some("needs the other file".into()))
        .await
        .unwrap();
    assert_eq!(fake.calls("nack"), ["nack 1"]);
    t.claim(None).await.unwrap();
    t.give_up("t2", "cannot be done".into()).await.unwrap();
    assert_eq!(fake.calls("dead_letter"), ["dead_letter 2"]);
    assert_eq!(
        fake.kinds(RUN),
        ["task_claimed", "task_released", "task_claimed", "task_dead"]
    );
    // Both are settled, so a new claim is allowed.
    assert_eq!(json(t.claim(None).await)["claimed"], false);
}

#[tokio::test]
async fn a_refused_operation_goes_on_the_log_and_the_model_sees_why() {
    let fake = Fake::default();
    fake.deliver(task_bytes("t1"), 1, 1);
    let t = tasks(&fake);
    t.claim(None).await.unwrap();
    fake.fail_on("cache_put_if");
    let err = t.complete("t1", "x".into()).await.unwrap_err();
    assert!(err.contains("permission denied"), "{err}");
    let log = fake.log(RUN);
    let Some(Record::Refused {
        op,
        resource,
        key,
        error,
    }) = log.last()
    else {
        panic!("last record: {:?}", log.last())
    };
    assert_eq!(
        (op.as_str(), resource.as_str(), key.as_deref()),
        ("cache_put_if", "agents.results.r1", Some("t1"))
    );
    assert!(error.contains("permission denied"));
    // The task is still held, so the worker can try again.
    fake.0.lock().unwrap().fail = None;
    assert_eq!(json(t.complete("t1", "x".into()).await)["status"], "done");
}
