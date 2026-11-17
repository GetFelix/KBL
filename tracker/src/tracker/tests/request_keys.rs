//! A write sent with a request key is written once: a retry with the key is
//! answered with the first write, whether the tracker restarted or not.

use kbl_core::content::Content;
use kbl_core::work::{HumanKind, RequestKey};

use super::*;
use crate::tracker::request_keys::WINDOW;

fn note(text: &str) -> WorkRecord {
    WorkRecord::HumanInput {
        item: item("a1"),
        attempt: None,
        input: HumanKind::Note,
        text: Content::Text(text.into()),
    }
}

/// Sends a person's note under `request`, as the REST route does.
async fn send(t: &Tracker<Fake>, request: &str, text: &str) -> Result<Value, Error> {
    let req = RequestKey::new(key(request), "/v1/work/note", &json!({ "text": text }));
    t.keyed(Actor::Person, &key("gab"), Some(req), async {
        t.person_record(key("gab"), note(text)).await
    })
    .await
}

fn notes(fake: &Fake) -> usize {
    fake.kinds().iter().filter(|k| *k == "human_input").count()
}

async fn ready(fake: &Fake) -> Tracker<Fake> {
    let t = start(fake).await;
    seed(&t, "a1", 2).await;
    t
}

#[tokio::test]
async fn a_retry_with_the_same_key_answers_with_the_first_write() {
    let fake = Fake::default();
    let t = ready(&fake).await;
    let first = send(&t, "r1", "look at this").await.unwrap();
    assert!(first.get("replayed").is_none());
    let again = send(&t, "r1", "look at this").await.unwrap();
    assert_eq!(again["replayed"], true);
    assert_eq!(again["offset"], first["offset"]);
    assert_eq!(again["kind"], "human_input");
    assert_eq!(again["item"], "a1");
    assert_eq!(notes(&fake), 1);
    // Another key is another write.
    send(&t, "r2", "look at this").await.unwrap();
    assert_eq!(notes(&fake), 2);
}

#[tokio::test]
async fn a_key_sent_with_another_body_is_a_conflict() {
    let fake = Fake::default();
    let t = ready(&fake).await;
    send(&t, "r1", "first").await.unwrap();
    let err = send(&t, "r1", "second").await.unwrap_err();
    assert!(
        matches!(&err, Error::Refused { why } if why == "request_key_reused"),
        "{err}"
    );
    assert_eq!(notes(&fake), 1);
}

#[tokio::test]
async fn a_retry_after_a_restart_is_still_answered_with_the_first_write() {
    let fake = Fake::default();
    let t = ready(&fake).await;
    let first = send(&t, "r1", "once").await.unwrap();
    drop(t);
    let t = start(&fake).await;
    let again = send(&t, "r1", "once").await.unwrap();
    assert_eq!(again["replayed"], true);
    assert_eq!(again["offset"], first["offset"]);
    assert_eq!(notes(&fake), 1);
    let err = send(&t, "r1", "other").await.unwrap_err();
    assert!(matches!(err, Error::Refused { .. }));
}

#[tokio::test]
async fn a_write_whose_answer_was_lost_is_not_written_again() {
    let fake = Fake::default();
    let t = ready(&fake).await;
    fake.lose_next_publish_reply("work.events");
    assert!(send(&t, "r1", "lost").await.is_err());
    // The record landed: the retry rebuilds from the log and finds the key.
    let again = send(&t, "r1", "lost").await.unwrap();
    assert_eq!(again["replayed"], true);
    assert_eq!(notes(&fake), 1);
}

#[tokio::test]
async fn a_key_is_remembered_for_a_bounded_window() {
    let fake = Fake::default();
    let t = ready(&fake).await;
    send(&t, "k-0", "n").await.unwrap();
    for i in 1..=WINDOW {
        send(&t, &format!("k-{i}"), "n").await.unwrap();
    }
    assert_eq!(notes(&fake), WINDOW + 1);
    // The newest is still there, and so after a restart; the oldest was forgotten.
    assert_eq!(
        send(&t, &format!("k-{WINDOW}"), "n").await.unwrap()["replayed"],
        true
    );
    let t = start(&fake).await;
    assert_eq!(
        send(&t, &format!("k-{WINDOW}"), "n").await.unwrap()["replayed"],
        true
    );
    assert!(
        send(&t, "k-0", "n")
            .await
            .unwrap()
            .get("replayed")
            .is_none()
    );
    assert_eq!(notes(&fake), WINDOW + 2);
}

#[tokio::test]
async fn one_callers_key_does_not_answer_for_another() {
    let fake = Fake::default();
    let t = ready(&fake).await;
    send(&t, "r1", "mine").await.unwrap();
    let req = RequestKey::new(key("r1"), "/v1/work/note", &json!({ "text": "mine" }));
    let other = t
        .keyed(Actor::Person, &key("kim"), Some(req), async {
            t.person_record(key("kim"), note("mine")).await
        })
        .await
        .unwrap();
    assert!(other.get("replayed").is_none());
    assert_eq!(notes(&fake), 2);
}

#[tokio::test]
async fn two_calls_with_one_key_at_once_write_once() {
    let fake = Fake::default();
    let t = ready(&fake).await;
    // The first call's record is on the log and its answer is held back.
    fake.hold_next_publish();
    let first = tokio::spawn({
        let t = t.clone();
        async move { send(&t, "r1", "x").await }
    });
    let landed = fake.log().len();
    while fake.log().len() == landed {
        tokio::task::yield_now().await;
    }
    let retry = send(&t, "r1", "x").await;
    assert!(matches!(retry, Err(Error::Unavailable(_))), "{retry:?}");
    fake.open_gate();
    first.await.unwrap().unwrap();
    assert_eq!(notes(&fake), 1);
    assert_eq!(send(&t, "r1", "x").await.unwrap()["replayed"], true);
}

#[tokio::test]
async fn an_agents_write_is_keyed_too() {
    let fake = Fake::default();
    let t = ready(&fake).await;
    claim(&t, "w1").await;
    let who = key("w1");
    let write = |text: &'static str| {
        let req = RequestKey::new(key("p1"), "/v1/work/progress", &json!({ "note": text }));
        let (t, who) = (t.clone(), who.clone());
        async move {
            t.keyed(Actor::Agent, &who, Some(req), async {
                t.progress(
                    &agent("w1"),
                    &item("a1"),
                    Content::Text(text.into()),
                    None,
                    None,
                )
                .await
            })
            .await
        }
    };
    let first = write("plan").await.unwrap();
    let again = write("plan").await.unwrap();
    assert_eq!(again["replayed"], true);
    assert_eq!(again["offset"], first["offset"]);
    assert_eq!(fake.kinds().iter().filter(|k| *k == "progress").count(), 1);
}
