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
    t.complete(&agent("w1"), &item("a1"), Vec::new())
        .await
        .unwrap();
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

/// Before format 4 the redo waits for green. From it, another agent gets it
/// while the request is still red.
#[tokio::test]
async fn a_redo_of_a_red_item_is_handed_out_once_the_format_is_raised() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let at = finished(&t).await;
    t.ingest_record(key("ingest-gh"), status(PrChecks::Red))
        .await
        .unwrap();
    let late = note(&t).await;
    redo(&t, at, Some(late)).await.unwrap();
    assert_eq!(claimed_id(&claim(&t, "w2").await), None);

    let raise = WorkRecord::FormatRaised {
        to: kbl_core::work::FORMAT_V4,
    };
    t.person_record(key("gab"), raise).await.unwrap();
    let got = claim(&t, "w2").await;
    assert_eq!(claimed_id(&got).as_deref(), Some("a1"));
    assert_eq!(got["attempt"], 2, "{got}");
}

/// A redo on a red item that is released goes back to Ready, and its retry is
/// handed out again instead of the item falling back to Needs you.
#[tokio::test]
async fn a_released_redo_of_a_red_item_can_be_claimed_again() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let at = finished(&t).await;
    t.person_record(
        key("gab"),
        WorkRecord::FormatRaised {
            to: kbl_core::work::FORMAT_V4,
        },
    )
    .await
    .unwrap();
    t.ingest_record(key("ingest-gh"), status(PrChecks::Red))
        .await
        .unwrap();
    let late = note(&t).await;
    redo(&t, at, Some(late)).await.unwrap();
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
    t.release(&agent("w2"), &item("a1"), None, Vec::new())
        .await
        .unwrap();

    let got = claim(&t, "w3").await;
    assert_eq!(claimed_id(&got).as_deref(), Some("a1"));
    assert_eq!(got["attempt"], 2, "{got}");
}

/// A redo made while the checks were still running names no note about the
/// failure that came after, so it waits for a note or for green.
#[tokio::test]
async fn a_redo_from_before_the_failure_waits_for_a_note() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let at = finished(&t).await;
    t.person_record(
        key("gab"),
        WorkRecord::FormatRaised {
            to: kbl_core::work::FORMAT_V4,
        },
    )
    .await
    .unwrap();
    let ingest = key("ingest-gh");
    t.ingest_record(ingest.clone(), status(PrChecks::Pending))
        .await
        .unwrap();
    redo(&t, at, None).await.unwrap();
    t.ingest_record(ingest.clone(), status(PrChecks::Red))
        .await
        .unwrap();
    assert_eq!(claimed_id(&claim(&t, "w2").await), None);

    note(&t).await;
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
}

async fn raise_to_4(t: &Tracker<Fake>) {
    let raise = WorkRecord::FormatRaised {
        to: kbl_core::work::FORMAT_V4,
    };
    t.person_record(key("gab"), raise).await.unwrap();
}

/// A waiting redo whose checks are re-run and fail again waits for a note on
/// the new failure; the person writes one and need not redo again.
#[tokio::test]
async fn a_waiting_redo_that_goes_red_again_is_freed_by_a_note() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let at = finished(&t).await;
    raise_to_4(&t).await;
    let ingest = key("ingest-gh");
    t.ingest_record(ingest.clone(), status(PrChecks::Red))
        .await
        .unwrap();
    let late = note(&t).await;
    redo(&t, at, Some(late)).await.unwrap();
    t.ingest_record(ingest.clone(), status(PrChecks::Pending))
        .await
        .unwrap();
    t.ingest_record(ingest, status(PrChecks::Red))
        .await
        .unwrap();
    assert_eq!(claimed_id(&claim(&t, "w2").await), None);

    note(&t).await;
    let got = claim(&t, "w2").await;
    assert_eq!(claimed_id(&got).as_deref(), Some("a1"));
    assert_eq!(got["attempt"], 2, "{got}");
}

/// A redo that pushed, went red again and was released is retried once a
/// person writes a note on the new failure.
#[tokio::test]
async fn a_claimed_redo_that_goes_red_again_is_retried_after_a_note() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let at = finished(&t).await;
    raise_to_4(&t).await;
    let ingest = key("ingest-gh");
    t.ingest_record(ingest.clone(), status(PrChecks::Red))
        .await
        .unwrap();
    let late = note(&t).await;
    redo(&t, at, Some(late)).await.unwrap();
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
    t.progress(
        &agent("w2"),
        &item("a1"),
        Content::Text("pushed".into()),
        None,
        Some(PR.into()),
    )
    .await
    .unwrap();
    t.ingest_record(ingest.clone(), status(PrChecks::Pending))
        .await
        .unwrap();
    t.ingest_record(ingest, status(PrChecks::Red))
        .await
        .unwrap();
    t.release(&agent("w2"), &item("a1"), None, Vec::new())
        .await
        .unwrap();
    assert_eq!(claimed_id(&claim(&t, "w3").await), None);

    note(&t).await;
    let got = claim(&t, "w3").await;
    assert_eq!(claimed_id(&got).as_deref(), Some("a1"));
    assert_eq!(got["attempt"], 2, "{got}");
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
