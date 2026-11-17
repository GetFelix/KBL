//! The review limit: at the cap agents take no new fix, and everything else carries on.

use kbl_core::work::{CloseWhy, WorkKind};

use super::*;

const LIMIT: usize = 2;

async fn limited() -> (Fake, Tracker<Fake>) {
    let fake = Fake::default();
    let t = start_with(
        &fake,
        Settings {
            review_limit: LIMIT,
            ..settings()
        },
    )
    .await;
    (fake, t)
}

/// `w1` fixes `id` and finishes, which puts it in In review.
async fn into_review(t: &Tracker<Fake>, id: &str) {
    seed(t, id, 1).await;
    assert_eq!(claimed_id(&claim(t, "w1").await).as_deref(), Some(id));
    t.complete(&agent("w1"), &item(id), None, Vec::new())
        .await
        .unwrap();
    assert_eq!(
        t.0.core.lock().await.state.stage(&item(id)),
        Some(Stage::InReview)
    );
}

async fn fill() -> (Fake, Tracker<Fake>) {
    let (fake, t) = limited().await;
    for i in 0..LIMIT {
        into_review(&t, &format!("r{i}")).await;
    }
    (fake, t)
}

#[tokio::test]
async fn at_the_limit_a_new_fix_is_not_claimed() {
    let (_fake, t) = fill().await;
    seed(&t, "new", 0).await;
    assert_eq!(claimed_id(&claim(&t, "w2").await), None);
}

#[tokio::test]
async fn below_the_limit_claims_resume() {
    let (_fake, t) = fill().await;
    seed(&t, "new", 0).await;
    assert_eq!(claimed_id(&claim(&t, "w2").await), None);
    // A merge or a close takes one out of In review; the held-back item is offered again.
    t.person_record(
        key("gab"),
        WorkRecord::Closed {
            item: item("r0"),
            why: CloseWhy::default(),
        },
    )
    .await
    .unwrap();
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("new"));
}

#[tokio::test]
async fn triage_is_still_claimable_at_the_limit() {
    let (_fake, t) = fill().await;
    seed(&t, "a1", 2).await;
    t.person_record(
        key("gab"),
        WorkRecord::Proposed {
            item: item("a1-t"),
            parent: Some(item("a1")),
            key: key("triage"),
            work: WorkKind::Triage,
            title: "Triage: fix a1".into(),
            review: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1-t"));
}

#[tokio::test]
async fn reviews_are_still_claimable_at_the_limit() {
    let fake = Fake::default();
    let t = start_with(
        &fake,
        Settings {
            review_limit: 1,
            ..settings()
        },
    )
    .await;
    seed_in_felix(&t, "a1").await;
    claim(&t, "w1").await;
    t.progress(
        &agent("w1"),
        &item("a1"),
        kbl_core::content::Content::Text("fixed".into()),
        None,
        Some("GetFelix/felix#a1".into()),
    )
    .await
    .unwrap();
    t.complete(&agent("w1"), &item("a1"), None, Vec::new())
        .await
        .unwrap();
    t.ingest_record(
        key("ingest-gh"),
        WorkRecord::PrStatus {
            item: item("a1"),
            status: kbl_core::work::PrSnapshot {
                pr: "GetFelix/felix#a1".into(),
                phase: kbl_core::work::PrPhase::Open,
                review: kbl_core::work::PrReview::Unreviewed,
                checks: kbl_core::work::PrChecks::Green,
                failing: None,
            },
        },
    )
    .await
    .unwrap();
    seed(&t, "new", 0).await;
    let tiers = vec!["fast".to_owned()];
    let got = t
        .next_for(&agent("w2"), Duration::ZERO, &tiers)
        .await
        .unwrap();
    assert_eq!(got["item"]["work"], "review");
}

#[tokio::test]
async fn only_review_and_held_fixes_count() {
    let (_fake, t) = limited().await;
    into_review(&t, "r0").await;
    // One in review is below the limit of two, whatever else is waiting.
    seed(&t, "b", 0).await;
    seed(&t, "c", 0).await;
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("b"));
    assert_eq!(t.0.core.lock().await.state.in_review_count(), 1);
    // The held `b` now counts with the one in review, so `c` waits.
    assert_eq!(claimed_id(&claim(&t, "w3").await), None);
}

#[tokio::test]
async fn a_limit_of_zero_is_off() {
    let fake = Fake::default();
    let t = start(&fake).await;
    for i in 0..5 {
        into_review(&t, &format!("r{i}")).await;
    }
    seed(&t, "new", 0).await;
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("new"));
}

#[tokio::test]
async fn a_fix_a_review_asked_for_is_claimed_at_the_limit() {
    let fake = Fake::default();
    let t = start_with(
        &fake,
        Settings {
            review_limit: 1,
            ..settings()
        },
    )
    .await;
    seed_in_felix(&t, "a1").await;
    claim(&t, "w1").await;
    t.progress(
        &agent("w1"),
        &item("a1"),
        kbl_core::content::Content::Text("fixed".into()),
        None,
        Some("GetFelix/felix#a1".into()),
    )
    .await
    .unwrap();
    t.complete(&agent("w1"), &item("a1"), None, Vec::new())
        .await
        .unwrap();
    t.ingest_record(
        key("ingest-gh"),
        WorkRecord::PrStatus {
            item: item("a1"),
            status: kbl_core::work::PrSnapshot {
                pr: "GetFelix/felix#a1".into(),
                phase: kbl_core::work::PrPhase::Open,
                review: kbl_core::work::PrReview::Unreviewed,
                checks: kbl_core::work::PrChecks::Green,
                failing: None,
            },
        },
    )
    .await
    .unwrap();
    let tiers = vec!["fast".to_owned()];
    let got = t
        .next_for(&agent("w2"), Duration::ZERO, &tiers)
        .await
        .unwrap();
    let review = item(got["item"]["id"].as_str().unwrap());
    t.finding(
        &agent("w2"),
        &review,
        crate::tracker::NewFinding {
            severity: kbl_core::work::Severity::High,
            file: "src/read.rs".into(),
            line: 88,
            title: "stale read".into(),
            scenario: "restart between fill and read".into(),
            verdict: kbl_core::work::FindingVerdict::Fix,
        },
    )
    .await
    .unwrap();
    t.complete(&agent("w2"), &review, None, Vec::new())
        .await
        .unwrap();
    // The PR is In review, so the limit of one is reached, yet its fix goes out.
    assert_eq!(t.0.core.lock().await.state.in_review_count(), 1);
    assert_eq!(claim(&t, "w4").await["item"]["work"], "fix");
}

#[tokio::test]
async fn fixes_agents_hold_count_toward_the_limit() {
    let (_fake, t) = limited().await;
    seed(&t, "h0", 0).await;
    seed(&t, "h1", 0).await;
    seed(&t, "h2", 0).await;
    assert!(claimed_id(&claim(&t, "w1").await).is_some());
    assert!(claimed_id(&claim(&t, "w2").await).is_some());
    // Two held with a limit of two: nothing is in review yet, and no third is offered.
    assert_eq!(t.0.core.lock().await.state.in_review_count(), 0);
    assert_eq!(claimed_id(&claim(&t, "w3").await), None);
}
