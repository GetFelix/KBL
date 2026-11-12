//! Reviews through the tracker: opened with the pull request, claimed by tier,
//! never by the author, and ended with findings and a verdict.

use kbl_core::content::Content;
use kbl_core::work::{
    FindingVerdict, MAX_FINDING_FILE, MAX_REVERT_TEST, MergeVerdict, Phase, PrChecks, PrPhase,
    PrReview, PrSnapshot, RevertCheck, Severity,
};
use kbl_core::work_fold::Stage;

use super::*;
use crate::tracker::{Decide, NewFinding};

const PR: &str = "GetFelix/felix#a1";

fn fast() -> Vec<String> {
    vec!["fast".to_owned()]
}

fn deep() -> Vec<String> {
    vec!["deep".to_owned()]
}

fn status(review: PrReview) -> WorkRecord {
    WorkRecord::PrStatus {
        item: item("a1"),
        status: PrSnapshot {
            pr: PR.into(),
            phase: PrPhase::Open,
            review,
            checks: PrChecks::Green,
            failing: None,
        },
    }
}

/// w1 fixes a1 and opens its request; ingest reports it open.
async fn opened() -> (Fake, Tracker<Fake>) {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed_in_felix(&t, "a1").await;
    claim(&t, "w1").await;
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
    t.ingest_record(key("ingest-gh"), status(PrReview::Unreviewed))
        .await
        .unwrap();
    (fake, t)
}

fn finding(verdict: FindingVerdict) -> NewFinding {
    NewFinding {
        severity: Severity::High,
        file: "src/read.rs".into(),
        line: 88,
        title: "stale read".into(),
        scenario: "restart between fill and read".into(),
        verdict,
    }
}

async fn fix_id(t: &Tracker<Fake>) -> String {
    let core = t.0.core.lock().await;
    core.state
        .items
        .values()
        .find(|i| i.finding.is_some() && i.parent.is_some())
        .map(|i| i.id.to_string())
        .unwrap()
}

async fn review_id(t: &Tracker<Fake>, tier: &str) -> ItemId {
    let core = t.0.core.lock().await;
    core.state
        .items
        .values()
        .find(|i| i.review.as_ref().is_some_and(|r| r.tier == tier))
        .map(|i| i.id.clone())
        .unwrap()
}

fn not_claimed(v: &Value) -> bool {
    !v["claimed"].as_bool().unwrap()
}

#[tokio::test]
async fn an_opened_request_gets_one_first_pass_and_one_final_review() {
    let (fake, t) = opened().await;
    assert_eq!(fake.kinds().iter().filter(|k| *k == "proposed").count(), 2);
    // The first pass is ready on the fast lane; the final waits for it.
    assert_eq!(fake.ready_count("work.review.r0"), 1);
    assert_eq!(fake.ready_count("work.review.r1"), 0);
    let first = review_id(&t, "fast").await;
    let last = review_id(&t, "deep").await;
    {
        let core = t.0.core.lock().await;
        assert_eq!(core.state.stage(&first), Some(Stage::Ready));
        assert_eq!(core.state.stage(&last), Some(Stage::Blocked));
    }
    // A later status for the same request opens nothing more.
    t.ingest_record(key("ingest-gh"), status(PrReview::Approved))
        .await
        .unwrap();
    assert_eq!(fake.kinds().iter().filter(|k| *k == "proposed").count(), 2);
}

/// A request opened while Felix refuses the writes after `ok` of them, then
/// reported again once it is back.
async fn opened_with_a_partial_write(ok: u32) -> (Fake, Tracker<Fake>) {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed_in_felix(&t, "a1").await;
    claim(&t, "w1").await;
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
    // The status record is one write, then the first pass, the final, the link.
    fake.fail_publishes_to_after(WORK_EVENTS, ok);
    t.ingest_record(key("ingest-gh"), status(PrReview::Unreviewed))
        .await
        .ok();
    fake.lift_publish_budget();
    t.ingest_record(key("ingest-gh"), status(PrReview::Approved))
        .await
        .unwrap();
    (fake, t)
}

#[tokio::test]
async fn a_partly_written_pair_of_reviews_is_finished_by_the_next_status() {
    // Cut off before the first pass, after it, and before the link.
    for ok in [1, 2, 3] {
        let (fake, t) = opened_with_a_partial_write(ok).await;
        assert_eq!(
            fake.kinds().iter().filter(|k| *k == "proposed").count(),
            2,
            "ok = {ok}"
        );
        let first = review_id(&t, "fast").await;
        let last = review_id(&t, "deep").await;
        let core = t.0.core.lock().await;
        assert!(core.state.is_linked(&last, &first), "ok = {ok}");
        assert_eq!(core.state.stage(&last), Some(Stage::Blocked), "ok = {ok}");
    }
}

#[tokio::test]
async fn reviews_are_claimed_only_by_naming_their_tier() {
    let (_fake, t) = opened().await;
    // No tier named: ordinary work only.
    assert!(not_claimed(&claim(&t, "w2").await));
    // The wrong tier.
    assert!(not_claimed(
        &t.next_for(&agent("w2"), Duration::ZERO, &deep())
            .await
            .unwrap()
    ));
    // An unknown tier, and more tiers than there are lanes, are refused.
    let slow = vec!["slow".to_owned()];
    assert!(matches!(
        t.next_for(&agent("w2"), Duration::ZERO, &slow).await,
        Err(Error::Invalid(_))
    ));
    let many: Vec<String> = (0..5).map(|i| format!("t{i}")).collect();
    assert!(
        t.next_for(&agent("w2"), Duration::ZERO, &many)
            .await
            .is_err()
    );

    let got = t
        .next_for(&agent("w2"), Duration::ZERO, &fast())
        .await
        .unwrap();
    assert_eq!(got["item"]["work"], "review");
    assert_eq!(got["item"]["review"]["tier"], "fast");
    // The review reads the root's request, which its author opened.
    assert_eq!(got["item"]["pr"], PR);
    assert_eq!(got["item"]["pr_branch"], "kibble/a1");
}

#[tokio::test]
async fn an_agent_is_never_given_a_review_of_its_own_pull_request() {
    let (_fake, t) = opened().await;
    let own = t
        .next_for(&agent("w1"), Duration::ZERO, &fast())
        .await
        .unwrap();
    assert!(not_claimed(&own));
    let other = t
        .next_for(&agent("w2"), Duration::ZERO, &fast())
        .await
        .unwrap();
    assert_eq!(other["item"]["work"], "review");
}

#[tokio::test]
async fn an_author_polling_for_reviews_does_not_use_up_the_review() {
    let (fake, t) = opened().await;
    // More polls than Felix's attempt bound.
    for _ in 0..8 {
        let own = t
            .next_for(&agent("w1"), Duration::ZERO, &fast())
            .await
            .unwrap();
        assert!(not_claimed(&own));
    }
    // The polls did not touch the record, or republish it.
    let calls = fake.calls();
    let on_lane = |p: &str| calls.iter().filter(|c| c.starts_with(p)).count();
    assert_eq!(on_lane("publish work.review"), 1);
    assert_eq!(on_lane("ack work.review"), 0);
    assert_eq!(on_lane("nack work.review"), 0);
    let other = t
        .next_for(&agent("w2"), Duration::ZERO, &fast())
        .await
        .unwrap();
    assert_eq!(other["item"]["work"], "review");
}

#[tokio::test]
async fn an_author_passing_over_its_review_does_not_hold_it_back_from_another() {
    // w1 wrote a1 (its review is first in the lane); w2 writes a2 (second).
    let (_fake, t) = opened().await;
    seed_in_felix(&t, "a2").await;
    let got = claim(&t, "w2").await;
    assert_eq!(claimed_id(&got).as_deref(), Some("a2"));
    t.progress(
        &agent("w2"),
        &item("a2"),
        Content::Text("fixed".into()),
        None,
        Some("GetFelix/felix#a2".into()),
    )
    .await
    .unwrap();
    t.complete(&agent("w2"), &item("a2"), 0).await.unwrap();
    t.ingest_record(
        key("ingest-gh"),
        WorkRecord::PrStatus {
            item: item("a2"),
            status: PrSnapshot {
                pr: "GetFelix/felix#a2".into(),
                phase: PrPhase::Open,
                review: PrReview::Unreviewed,
                checks: PrChecks::Green,
                failing: None,
            },
        },
    )
    .await
    .unwrap();
    // w1 passes over its own review and takes w2's.
    let theirs = t
        .next_for(&agent("w1"), Duration::ZERO, &fast())
        .await
        .unwrap();
    assert_eq!(theirs["item"]["work"], "review");
    assert_eq!(theirs["item"]["pr"], "GetFelix/felix#a2");
    // w2 can take w1's review at once, with no wait for the pass-over to expire.
    let mine = t
        .next_for(&agent("w2"), Duration::ZERO, &fast())
        .await
        .unwrap();
    assert_eq!(mine["item"]["pr"], PR);
}

/// w1 wrote a1 and a2 was opened first, so a2's review is the other agents' item
/// and sits first in the lane; w1's own review of a1 follows it.
async fn author_beside_a_review_it_cannot_take() -> (Fake, Tracker<Fake>) {
    let fake = Fake::default();
    let t = start(&fake).await;
    for (id, who) in [("a2", "w2"), ("a1", "w1")] {
        seed_in_felix(&t, id).await;
        claim(&t, who).await;
        let pr = format!("GetFelix/felix#{id}");
        t.progress(
            &agent(who),
            &item(id),
            Content::Text("fixed".into()),
            None,
            Some(pr.clone()),
        )
        .await
        .unwrap();
        t.complete(&agent(who), &item(id), 0).await.unwrap();
        t.ingest_record(
            key("ingest-gh"),
            WorkRecord::PrStatus {
                item: item(id),
                status: PrSnapshot {
                    pr,
                    phase: PrPhase::Open,
                    review: PrReview::Unreviewed,
                    checks: PrChecks::Green,
                    failing: None,
                },
            },
        )
        .await
        .unwrap();
    }
    (fake, t)
}

async fn author_polls_ten_times(fake: &Fake, t: &Tracker<Fake>) -> usize {
    for _ in 0..10 {
        let got = t
            .next_for(&agent("w1"), Duration::ZERO, &fast())
            .await
            .unwrap();
        assert!(not_claimed(&got));
    }
    fake.calls()
        .iter()
        .filter(|c| c.starts_with("publish work.review"))
        .count()
}

#[tokio::test]
async fn an_author_does_not_churn_beside_a_review_leased_to_someone_else() {
    let (fake, t) = author_beside_a_review_it_cannot_take().await;
    // w3 took the other review and its work_next was cut short: the lease stays.
    fake.poll_and_vanish(&ready_stream(TIERS), "w3").unwrap();
    // Two first publishes, one republish of w1's own review, then quiet.
    assert!(author_polls_ten_times(&fake, &t).await <= 3);
}

#[tokio::test]
async fn an_author_does_not_churn_beside_a_dead_lettered_review() {
    let (fake, t) = author_beside_a_review_it_cannot_take().await;
    fake.net
        .lock()
        .unwrap()
        .slots
        .insert((ready_stream(TIERS), 0), Slot::Acked);
    assert!(author_polls_ten_times(&fake, &t).await <= 3);
}

#[tokio::test]
async fn findings_become_children_and_proposals_and_a_verdict_ends_the_final_review() {
    let (fake, t) = opened().await;
    let got = t
        .next_for(&agent("w2"), Duration::ZERO, &fast())
        .await
        .unwrap();
    let first = item(got["item"]["id"].as_str().unwrap());
    for v in [
        FindingVerdict::Fix,
        FindingVerdict::Defer,
        FindingVerdict::Dismiss,
    ] {
        t.finding(&agent("w2"), &first, finding(v)).await.unwrap();
    }
    {
        let core = t.0.core.lock().await;
        let kids: Vec<_> = core
            .state
            .items
            .values()
            .filter(|i| i.finding.is_some())
            .collect();
        assert_eq!(kids.len(), 2);
        let fixed = kids.iter().find(|i| i.parent.is_some()).unwrap();
        assert_eq!(core.state.stage(&fixed.id), Some(Stage::Ready));
        let deferred = kids.iter().find(|i| i.parent.is_none()).unwrap();
        assert_eq!(core.state.stage(&deferred.id), Some(Stage::Proposed));
    }
    t.complete(&agent("w2"), &first, 0).await.unwrap();

    // The final review waits for the fix the first pass asked for.
    assert_eq!(claim(&t, "w4").await["item"]["work"], "fix");
    let early = t
        .next_for(&agent("w3"), Duration::ZERO, &deep())
        .await
        .unwrap();
    assert!(not_claimed(&early), "{early}");
    t.complete(&agent("w4"), &item(&fix_id(&t).await), 0)
        .await
        .unwrap();
    let fin = t
        .next_for(&agent("w3"), Duration::ZERO, &deep())
        .await
        .unwrap();
    let last = item(fin["item"]["id"].as_str().unwrap());
    t.review_verdict(&agent("w3"), &last, MergeVerdict::Merge, "good".into())
        .await
        .unwrap();
    t.complete(&agent("w3"), &last, 0).await.unwrap();
    let core = t.0.core.lock().await;
    let v = core.state.items[&item("a1")]
        .review_verdict
        .clone()
        .unwrap();
    assert_eq!(v.verdict, MergeVerdict::Merge);
    assert!(core.state.merge_ready(&item("a1")));
    // Merging stays a person's action: nothing here closed the root.
    assert!(core.state.items[&item("a1")].closed.is_none());
    assert!(!fake.kinds().contains(&"closed".to_owned()));
}

#[tokio::test]
async fn findings_and_verdicts_are_bounded_before_they_reach_the_log() {
    let (fake, t) = opened().await;
    let got = t
        .next_for(&agent("w2"), Duration::ZERO, &fast())
        .await
        .unwrap();
    let first = item(got["item"]["id"].as_str().unwrap());
    let before = fake.log().len();
    let mut long = finding(FindingVerdict::Fix);
    long.file = "f".repeat(MAX_FINDING_FILE + 1);
    assert!(matches!(
        t.finding(&agent("w2"), &first, long).await,
        Err(Error::Invalid(_))
    ));
    let mut no_line = finding(FindingVerdict::Fix);
    no_line.line = 0;
    assert!(matches!(
        t.finding(&agent("w2"), &first, no_line).await,
        Err(Error::Invalid(_))
    ));
    // Not the holder.
    assert!(
        t.finding(&agent("w3"), &first, finding(FindingVerdict::Fix))
            .await
            .is_err()
    );
    // A first pass gives no verdict.
    let err = t
        .review_verdict(&agent("w2"), &first, MergeVerdict::Merge, "x".into())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Refused { why } if why == "not_review"));
    assert_eq!(fake.log().len(), before);
}

fn decide(revert_check: Option<RevertCheck>) -> Decide {
    Decide {
        phase: Phase::Verify,
        so_far: "fix in, test added".into(),
        question: String::new(),
        options: vec![],
        chosen: String::new(),
        why: "the test fails without the fix".into(),
        commit: "abc123".into(),
        evidence: vec![],
        revert_check,
    }
}

#[tokio::test]
async fn a_revert_check_rides_on_the_decision_within_bounds() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    let check = |test: &str| {
        Some(RevertCheck {
            test: test.into(),
            failed_without_fix: true,
            passed_with_fix: true,
        })
    };
    t.decide(
        &agent("w1"),
        &item("a1"),
        decide(check("reads_see_a_write")),
    )
    .await
    .unwrap();
    let long = "t".repeat(MAX_REVERT_TEST + 1);
    assert!(matches!(
        t.decide(&agent("w1"), &item("a1"), decide(check(&long)))
            .await,
        Err(Error::Invalid(_))
    ));
    let core = t.0.core.lock().await;
    assert_eq!(core.state.items[&item("a1")].paths[0].proven.len(), 1);
}

#[tokio::test]
async fn reviewer_tiers_must_be_names_that_fit_the_lanes() {
    for tiers in [
        vec![],
        vec!["Fast".to_owned()],
        vec!["a".to_owned(), "a".to_owned()],
        (0..5).map(|i| format!("t{i}")).collect(),
    ] {
        let fake = Fake::default();
        fake.expire_lease();
        let bad = Settings {
            review_tiers: tiers,
            ..settings()
        };
        assert!(Tracker::start(fake.reconnect(), bad).await.is_err());
    }
}
