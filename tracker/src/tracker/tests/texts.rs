//! Texts the cache let go are read back from the log without the core lock,
//! and what the read finds is told apart.

use std::time::Duration;

use super::*;

async fn ingest_root(t: &Tracker<Fake>) {
    let r = WorkRecord::Imported {
        item: item("r1"),
        source: "github".into(),
        upstream_id: "GetFelix/felix#1".into(),
        title: "a bug".into(),
    };
    t.ingest_record(key("ingest-gh"), r).await.unwrap();
}

fn context(entry: &str, body: &str) -> WorkRecord {
    WorkRecord::Context {
        item: item("r1"),
        entry: key(entry),
        text: Content::Text(body.into()),
    }
}

/// Writes `issue`, then another entry with the cache cap so low that the
/// first text is let go. Returns the offset of `issue`.
async fn evicted_issue(t: &Tracker<Fake>) -> u64 {
    ingest_root(t).await;
    let at = t
        .ingest_record(key("ingest-gh"), context("issue", "the body"))
        .await
        .unwrap()["offset"]
        .as_u64()
        .unwrap();
    t.0.core.lock().await.recall.texts.set_cap(1);
    t.ingest_record(key("ingest-gh"), context("other", "more"))
        .await
        .unwrap();
    assert!(t.0.core.lock().await.recall.texts.peek(at).is_none());
    at
}

#[tokio::test]
async fn a_text_miss_does_not_hold_the_core_lock_across_the_read() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    for n in 0..3 {
        t.context_add(
            &agent("w1"),
            &item("a1"),
            key(&format!("e{n}")),
            Content::Text(format!("{n}{}", "x".repeat(100))),
        )
        .await
        .unwrap();
    }
    t.0.core.lock().await.recall.texts.set_cap(150);
    t.context_add(
        &agent("w1"),
        &item("a1"),
        key("e3"),
        Content::Text("y".repeat(100)),
    )
    .await
    .unwrap();
    fake.hold_next_read();
    let reader = {
        let t = t.clone();
        tokio::spawn(async move { t.context_read(&agent("w1"), &item("a1")).await })
    };
    // The read is stuck in Felix; a write must still get the lock.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (who, on) = (agent("w1"), item("a1"));
    let write = t.context_add(&who, &on, key("e4"), Content::Text("z".into()));
    tokio::time::timeout(Duration::from_secs(2), write)
        .await
        .expect("the write waited on the read")
        .unwrap();
    fake.gate.notify_one();
    let read = reader.await.unwrap().unwrap();
    assert_eq!(read["context"].as_array().unwrap().len(), 5);
    assert_eq!(
        read["context"][0]["text"],
        json!(Content::Text(format!("0{}", "x".repeat(100))))
    );
}

#[tokio::test]
async fn ingest_does_not_write_an_entry_again_when_the_text_to_compare_was_trimmed() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let at = evicted_issue(&t).await;
    fake.trim_before(WORK_EVENTS, at + 1);
    let before = fake.log().len();
    let again = t
        .ingest_record(key("ingest-gh"), context("issue", "the body"))
        .await
        .unwrap();
    assert_eq!(again["unchanged"], true);
    assert_eq!(fake.log().len(), before);
}

#[tokio::test]
async fn ingest_fails_and_writes_nothing_when_the_text_cannot_be_read() {
    let fake = Fake::default();
    let t = start(&fake).await;
    evicted_issue(&t).await;
    let before = fake.log().len();
    fake.fail_reads(true);
    let err = t
        .ingest_record(key("ingest-gh"), context("issue", "the body"))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Unavailable(_)), "{err}");
    fake.fail_reads(false);
    assert_eq!(fake.log().len(), before);
    // Readable again, the same text is recognised and an edit is written.
    t.ingest_record(key("ingest-gh"), context("issue", "the body"))
        .await
        .unwrap();
    assert_eq!(fake.log().len(), before);
    t.ingest_record(key("ingest-gh"), context("issue", "an edit"))
        .await
        .unwrap();
    assert_eq!(fake.log().len(), before + 1);
}

#[tokio::test]
async fn reading_texts_tells_text_from_no_text_and_trimmed() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let at = evicted_issue(&t).await;
    // The import record has no text.
    let found = t.read_texts(&[0, at]).await.unwrap();
    assert!(matches!(found[&0], TextRead::NoText));
    assert!(matches!(&found[&at], TextRead::Text(Content::Text(b)) if b == "the body"));
    fake.trim_before(WORK_EVENTS, at);
    let found = t.read_texts(&[0, at, 99]).await.unwrap();
    assert!(matches!(found[&0], TextRead::Gone));
    assert!(matches!(found[&at], TextRead::Text(_)));
    assert!(matches!(found[&99], TextRead::Gone));
}

#[tokio::test]
async fn texts_are_found_across_pages_and_raft_blanks() {
    let fake = Fake::default();
    fake.raft_blanks();
    let t = start(&fake).await;
    ingest_root(&t).await;
    let mut wanted = Vec::new();
    for n in 0..3 {
        let at = t
            .ingest_record(
                key("ingest-gh"),
                context(&format!("e{n}"), &format!("body {n}")),
            )
            .await
            .unwrap()["offset"]
            .as_u64()
            .unwrap();
        wanted.push(at);
        fake.blank(WORK_EVENTS);
        fake.blank(WORK_EVENTS);
        // A write after a blank lands past it.
        let _ = t
            .ingest_record(key("ingest-gh"), context("pad", &format!("pad {n}")))
            .await;
    }
    fake.page_size(1);
    let found = t.read_texts(&wanted).await.unwrap();
    for (n, at) in wanted.iter().enumerate() {
        assert!(
            matches!(&found[at], TextRead::Text(Content::Text(b)) if *b == format!("body {n}")),
            "offset {at}"
        );
    }
    // An offset that is a blank holds no record.
    let blank = *fake.net.lock().unwrap().blanks[WORK_EVENTS]
        .iter()
        .next()
        .unwrap();
    let found = t.read_texts(&[blank]).await.unwrap();
    assert!(matches!(found[&blank], TextRead::Gone));
}
