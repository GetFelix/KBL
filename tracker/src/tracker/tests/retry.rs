//! A redo of an item whose pull request has a failed check names a note
//! written since the check failed.

use kbl_core::content::Content;
use kbl_core::work::{FailedCheck, HumanKind, Phase, PrChecks, PrPhase, PrReview, PrSnapshot};

use super::*;
use crate::tracker::Decide;

const PR: &str = "GetFelix/felix#a1";

fn status(checks: PrChecks) -> WorkRecord {
    WorkRecord::PrStatus {
        item: item("a1"),
        status: PrSnapshot {
            pr: PR.into(),
            phase: PrPhase::Open,
            review: PrReview::Unreviewed,
            checks,
            failing: (checks == PrChecks::Red).then(|| FailedCheck {
                name: "test".into(),
                url: "https://ci.example/1".into(),
            }),
        },
    }
}

/// w1 decides, opens a request and finishes a1. The decision's offset.
async fn finished(t: &Tracker<Fake>) -> u64 {
    seed_in_felix(t, "a1").await;
    claim(t, "w1").await;
    let d = Decide {
        phase: Phase::Verify,
        so_far: "fix in".into(),
        question: String::new(),
        options: vec![],
        chosen: String::new(),
        why: "the test passes".into(),
        commit: "abc123".into(),
        evidence: vec![],
        revert_check: None,
    };
    let at = t.decide(&agent("w1"), &item("a1"), d).await.unwrap()["offset"]
        .as_u64()
        .unwrap();
    t.progress(
        &agent("w1"),
        &item("a1"),
        Content::Text("fixed".into()),
        None,
        Some(PR.into()),
    )
    .await
    .unwrap();
    t.complete(&agent("w1"), &item("a1"), 0).await.unwrap();
    at
}

async fn note(t: &Tracker<Fake>) -> u64 {
    let r = WorkRecord::HumanInput {
        item: item("a1"),
        attempt: None,
        input: HumanKind::Note,
        text: Content::Text("the reader test times out; raise its bound".into()),
    };
    t.person_record(key("gab"), r).await.unwrap()["offset"]
        .as_u64()
        .unwrap()
}

async fn redo(t: &Tracker<Fake>, at: u64, note: Option<u64>) -> Result<Value, Error> {
    let r = WorkRecord::ForkedFrom {
        item: item("a1"),
        from_attempt: 1,
        at,
        note,
    };
    t.person_record(key("gab"), r).await
}

fn note_required(r: Result<Value, Error>) -> bool {
    matches!(r, Err(Error::Refused { ref why }) if why == "note_required")
}

#[tokio::test]
async fn a_redo_after_a_failed_check_names_a_note_written_since_it_failed() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let at = finished(&t).await;
    let early = note(&t).await;
    let ingest = key("ingest-gh");
    t.ingest_record(ingest.clone(), status(PrChecks::Red))
        .await
        .unwrap();
    let before = fake.log().len();
    assert!(note_required(redo(&t, at, None).await));
    // A note from before the failure says nothing about it.
    assert!(note_required(redo(&t, at, Some(early)).await));
    assert_eq!(fake.log().len(), before);

    let late = note(&t).await;
    // The same failure reported again does not move the mark past the note.
    t.ingest_record(ingest, status(PrChecks::Red))
        .await
        .unwrap();
    redo(&t, at, Some(late)).await.unwrap();
    assert_eq!(fake.kinds().last().map(String::as_str), Some("forked_from"));
}

#[tokio::test]
async fn a_restart_keeps_when_the_check_failed() {
    let fake = Fake::default();
    let first = start(&fake).await;
    let at = finished(&first).await;
    let early = note(&first).await;
    first
        .ingest_record(key("ingest-gh"), status(PrChecks::Red))
        .await
        .unwrap();
    drop(first);

    let second = start(&fake).await;
    assert!(note_required(redo(&second, at, Some(early)).await));
    let late = note(&second).await;
    redo(&second, at, Some(late)).await.unwrap();
}

#[tokio::test]
async fn a_redo_needs_no_note_while_no_check_has_failed() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let at = finished(&t).await;
    let ingest = key("ingest-gh");
    // Red and green again: the failure is over.
    t.ingest_record(ingest.clone(), status(PrChecks::Red))
        .await
        .unwrap();
    t.ingest_record(ingest, status(PrChecks::Green))
        .await
        .unwrap();
    redo(&t, at, None).await.unwrap();
}

#[tokio::test]
async fn a_closed_item_gets_the_folds_refusal_not_a_request_for_a_note() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let at = finished(&t).await;
    t.ingest_record(key("ingest-gh"), status(PrChecks::Red))
        .await
        .unwrap();
    let closed = WorkRecord::Closed {
        item: item("a1"),
        why: kbl_core::work::CloseWhy::Dismissed,
    };
    t.person_record(key("gab"), closed).await.unwrap();
    let r = redo(&t, at, None).await;
    assert!(
        matches!(r, Err(Error::Refused { ref why }) if why == "closed"),
        "{r:?}"
    );
}

#[test]
fn another_request_on_the_item_starts_a_new_failure() {
    let progress = |pr: &str| WorkRecord::Progress {
        item: item("a1"),
        attempt: 2,
        note: Content::Text("opened".into()),
        branch: None,
        pr: Some(pr.into()),
    };
    let mut recall = Recall::default();
    recall.remember(10, &status(PrChecks::Red));
    // Reported again, the request keeps its mark.
    recall.remember(15, &progress(PR));
    assert_eq!(recall.red_since[&item("a1")].1, 10);
    // Away to another request and back: the old failure is over.
    recall.remember(20, &progress("GetFelix/felix#b2"));
    recall.remember(30, &progress(PR));
    recall.remember(40, &status(PrChecks::Red));
    assert_eq!(recall.red_since[&item("a1")].1, 40);
}
