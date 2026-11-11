//! Reviews as work: who may create and claim them, and what findings and a
//! verdict do.

use super::*;
use crate::work::{
    FindingVerdict, MAX_FINDING_FILE, MAX_FINDING_TITLE, MAX_SCENARIO, MAX_VERDICT_WHY,
    MergeVerdict, PrSnapshot, RevertCheck, ReviewPass, ReviewSpec, Severity,
};

const PR: &str = "GetFelix/felix#9";

fn spec(pass: ReviewPass, tier: &str) -> ReviewSpec {
    ReviewSpec {
        pass,
        tier: tier.into(),
        pr: PR.into(),
    }
}

fn review_rec(item: &str, pass: ReviewPass, tier: &str) -> WorkRecord {
    WorkRecord::Proposed {
        item: id(item),
        parent: Some(id("r1")),
        key: key(item),
        work: WorkKind::Review,
        title: format!("review {item}"),
        review: Some(spec(pass, tier)),
    }
}

/// A root that w1 fixed and opened `PR` for; the tracker then opened a
/// first-pass review `v1` and a final review `v2` that waits for it.
fn reviewed() -> Log {
    let mut l = Log::default();
    l.tracker(WorkRecord::Imported {
        item: id("r1"),
        source: "github".into(),
        upstream_id: PR.into(),
        title: "bug".into(),
    });
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
    });
    l.agent("w1", claimed("r1", 1));
    l.agent(
        "w1",
        WorkRecord::Progress {
            item: id("r1"),
            attempt: 1,
            note: text("fixed"),
            branch: None,
            pr: Some(PR.into()),
        },
    );
    l.agent("w1", done("r1", 1, 0));
    l.tracker(WorkRecord::PrStatus {
        item: id("r1"),
        status: PrSnapshot {
            pr: PR.into(),
            phase: PrPhase::Open,
            review: PrReview::Unreviewed,
            checks: PrChecks::Green,
            failing: None,
        },
    });
    l.tracker(review_rec("v1", ReviewPass::First, "fast"));
    l.tracker(review_rec("v2", ReviewPass::Final, "deep"));
    l.tracker(WorkRecord::Linked {
        item: id("v2"),
        to: id("v1"),
    });
    l
}

fn finding(item: &str, verdict: FindingVerdict) -> WorkRecord {
    WorkRecord::Finding {
        item: id(item),
        attempt: 1,
        severity: Severity::High,
        file: "src/read.rs".into(),
        line: 88,
        title: "stale read".into(),
        scenario: "restart between fill and read".into(),
        verdict,
    }
}

fn verdict(item: &str, v: MergeVerdict) -> WorkRecord {
    WorkRecord::ReviewVerdict {
        item: id(item),
        attempt: 1,
        verdict: v,
        why: "reads fine".into(),
    }
}

fn last(l: &Log) -> Option<Reject> {
    last_rejection(l)
}

fn state(l: &Log) -> State {
    fold(&l.0)
}

#[test]
fn only_the_tracker_opens_reviews_and_only_for_the_reported_request() {
    let l = reviewed();
    assert_eq!(state(&l).rejections.len(), 0);
    assert_eq!(state(&l).stage(&id("v1")), Some(Stage::Ready));
    // A final review waits for the first.
    assert_eq!(state(&l).stage(&id("v2")), Some(Stage::Blocked));

    for (actor, who) in [(Actor::Agent, "w1"), (Actor::Person, "gabriel")] {
        let mut l = reviewed();
        l.push(who, actor, review_rec("v3", ReviewPass::First, "fast"));
        assert_eq!(last(&l), Some(Reject::ActorNotAllowed));
    }

    // A request the root does not report.
    let mut l = reviewed();
    let mut other = review_rec("v3", ReviewPass::First, "fast");
    if let WorkRecord::Proposed {
        review: Some(r), ..
    } = &mut other
    {
        r.pr = "GetFelix/felix#10".into();
    }
    l.tracker(other);
    assert_eq!(last(&l), Some(Reject::WrongPr));

    // A review kind and its spec go together, and the tier is a plain name.
    let mut l = reviewed();
    let mut plain = review_rec("v3", ReviewPass::First, "fast");
    if let WorkRecord::Proposed { review, .. } = &mut plain {
        *review = None;
    }
    l.tracker(plain);
    assert_eq!(last(&l), Some(Reject::MissingField));
    let mut l = reviewed();
    l.tracker(review_rec("v3", ReviewPass::First, "Deep Tier"));
    assert_eq!(last(&l), Some(Reject::MissingField));
}

#[test]
fn a_root_has_a_bounded_number_of_reviews() {
    let mut l = reviewed();
    for i in 0..MAX_REVIEWS_PER_ROOT - 2 {
        l.tracker(review_rec(&format!("x{i}"), ReviewPass::First, "fast"));
    }
    assert_eq!(state(&l).rejections.len(), 0);
    l.tracker(review_rec("over", ReviewPass::First, "fast"));
    assert_eq!(last(&l), Some(Reject::LimitReached));
}

#[test]
fn an_agent_never_reviews_the_pull_request_it_worked_on() {
    let mut l = reviewed();
    l.agent("w1", claimed("v1", 1));
    assert_eq!(last(&l), Some(Reject::SelfReview));
    assert_eq!(state(&l).stage(&id("v1")), Some(Stage::Ready));
    l.agent("w2", claimed("v1", 1));
    assert_eq!(state(&l).rejections.len(), 1);
    assert_eq!(state(&l).stage(&id("v1")), Some(Stage::Held));
}

#[test]
fn an_agent_that_fixed_a_finding_cannot_give_the_final_review() {
    let mut l = reviewed();
    l.agent("w2", claimed("v1", 1));
    l.agent("w2", finding("v1", FindingVerdict::Fix));
    l.agent("w2", done("v1", 1, 0));
    let fix = state(&l).items[&id("r1")].children.last().unwrap().clone();
    l.agent("w3", claimed(fix.as_str(), 1));
    l.agent("w3", done(fix.as_str(), 1, 0));
    assert_eq!(state(&l).rejections.len(), 0);
    l.agent("w3", claimed("v2", 1));
    assert_eq!(last(&l), Some(Reject::SelfReview));
    l.agent("w2", claimed("v2", 1));
    assert_eq!(state(&l).stage(&id("v2")), Some(Stage::Held));
}

#[test]
fn a_fix_finding_becomes_a_child_and_a_deferred_one_a_proposal() {
    let mut l = reviewed();
    l.agent("w2", claimed("v1", 1));
    let at_fix = l.agent("w2", finding("v1", FindingVerdict::Fix));
    let at_defer = l.agent("w2", finding("v1", FindingVerdict::Defer));
    l.agent("w2", finding("v1", FindingVerdict::Dismiss));
    let s = state(&l);
    assert_eq!(s.rejections.len(), 0);
    assert_eq!(s.items[&id("v1")].findings.len(), 3);

    let fix = s
        .items
        .values()
        .find(|i| i.finding == Some(at_fix))
        .expect("a fix child");
    assert_eq!(fix.parent, Some(id("r1")));
    assert_eq!(fix.work, WorkKind::Fix);
    assert!(fix.accepted);
    assert_eq!(s.stage(&fix.id), Some(Stage::Ready));
    // It draws on the root's budget like any child.
    assert_eq!(s.budgets[&id("r1")].items_left, DEFAULT_MAX_ITEMS - 2);

    let defer = s
        .items
        .values()
        .find(|i| i.finding == Some(at_defer))
        .expect("a proposal");
    assert_eq!(defer.parent, None);
    assert!(defer.proposed_by_agent);
    assert!(!defer.accepted);
    assert_eq!(s.stage(&defer.id), Some(Stage::Proposed));
    assert_eq!(
        defer.target.as_ref().map(|t| t.repo.as_str()),
        Some("GetFelix/felix")
    );
    // A person accepts it into the queue or rejects it.
    assert_eq!(s.items.len(), 5);
}

#[test]
fn a_finding_is_bounded_and_belongs_to_the_holder_of_a_review() {
    let bad = |f: fn(&mut WorkRecord)| {
        let mut l = reviewed();
        l.agent("w2", claimed("v1", 1));
        let mut r = finding("v1", FindingVerdict::Dismiss);
        f(&mut r);
        l.agent("w2", r);
        last(&l)
    };
    assert_eq!(
        bad(|r| {
            let long = "f".repeat(MAX_FINDING_FILE + 1);
            if let WorkRecord::Finding { file, .. } = r {
                *file = long;
            }
        }),
        Some(Reject::TooLong)
    );
    assert_eq!(
        bad(|r| {
            let long = "t".repeat(MAX_FINDING_TITLE + 1);
            if let WorkRecord::Finding { title, .. } = r {
                *title = long;
            }
        }),
        Some(Reject::TooLong)
    );
    assert_eq!(
        bad(|r| {
            let long = "s".repeat(MAX_SCENARIO + 1);
            if let WorkRecord::Finding { scenario, .. } = r {
                *scenario = long;
            }
        }),
        Some(Reject::TooLong)
    );
    assert_eq!(
        bad(|r| {
            if let WorkRecord::Finding { file, .. } = r {
                file.clear();
            }
        }),
        Some(Reject::MissingField)
    );
    assert_eq!(
        bad(|r| {
            if let WorkRecord::Finding { line, .. } = r {
                *line = 0;
            }
        }),
        Some(Reject::MissingField)
    );

    // Not the holder, not an agent, and not a review.
    let mut l = reviewed();
    l.agent("w2", claimed("v1", 1));
    l.agent("w3", finding("v1", FindingVerdict::Dismiss));
    assert_eq!(last(&l), Some(Reject::NotHolder));
    l.person(finding("v1", FindingVerdict::Dismiss));
    assert_eq!(last(&l), Some(Reject::ActorNotAllowed));
    let mut l = reviewed();
    l.agent("w2", proposed("t9", Some("r1"), "x", WorkKind::Test));
    assert!(!state(&l).rejections.is_empty());
    let mut l = Log::default();
    accepted_root(&mut l, "r2");
    l.agent("w2", claimed("r2", 1));
    l.agent("w2", finding("r2", FindingVerdict::Dismiss));
    assert_eq!(last(&l), Some(Reject::NotReview));
}

#[test]
fn a_review_records_a_bounded_number_of_findings() {
    let mut l = reviewed();
    l.agent("w2", claimed("v1", 1));
    for _ in 0..MAX_FINDINGS {
        l.agent("w2", finding("v1", FindingVerdict::Dismiss));
    }
    assert_eq!(state(&l).rejections.len(), 0);
    l.agent("w2", finding("v1", FindingVerdict::Dismiss));
    assert_eq!(last(&l), Some(Reject::LimitReached));
    assert_eq!(
        state(&l).items[&id("v1")].findings.len(),
        MAX_FINDINGS as usize
    );
}

#[test]
fn fix_findings_stop_at_the_roots_budget() {
    let mut l = reviewed();
    l.person(WorkRecord::BudgetSet {
        root: id("r1"),
        items_left: 1,
        max_depth: 3,
        cost_left_micros: 1_000_000,
        redos_per_item: 3,
    });
    l.agent("w2", claimed("v1", 1));
    l.agent("w2", finding("v1", FindingVerdict::Fix));
    assert_eq!(state(&l).rejections.len(), 0);
    l.agent("w2", finding("v1", FindingVerdict::Fix));
    assert_eq!(last(&l), Some(Reject::OverBudget));
    assert_eq!(state(&l).items[&id("v1")].findings.len(), 1);
}

#[test]
fn a_stale_review_cannot_speak_for_a_newer_request() {
    let mut l = reviewed();
    l.agent("w2", claimed("v1", 1));
    l.person(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Dismissed,
    });
    l.agent("w2", finding("v1", FindingVerdict::Dismiss));
    // Closing the root cancelled the review, so nobody holds it any more.
    assert_eq!(last(&l), Some(Reject::NotHolder));
}

/// A final review claimed by w3, with the first pass done by w2.
fn at_final() -> Log {
    let mut l = reviewed();
    l.agent("w2", claimed("v1", 1));
    l.agent("w2", done("v1", 1, 0));
    l.agent("w3", claimed("v2", 1));
    l
}

#[test]
fn the_final_review_ends_with_a_verdict_a_person_acts_on() {
    let mut l = at_final();
    // It cannot finish without one.
    l.agent("w3", done("v2", 1, 0));
    assert_eq!(last(&l), Some(Reject::MissingField));
    l.agent("w3", verdict("v2", MergeVerdict::Merge));
    l.agent("w3", done("v2", 1, 0));
    let s = state(&l);
    assert_eq!(s.rejections.len(), 1);
    let v = s.items[&id("r1")].review_verdict.clone().unwrap();
    assert_eq!(v.verdict, MergeVerdict::Merge);
    assert_eq!(v.review, id("v2"));
    assert_eq!(v.pr, PR);
    // Merging is still a person's action: the item waits in review.
    assert!(s.merge_ready(&id("r1")));
    assert_eq!(s.stage(&id("r1")), Some(Stage::InReview));
}

/// The id of the fix the finding at `offset` of `review` makes.
fn fix_of(l: &Log, review: &str) -> ItemId {
    let s = state(l);
    let off = s.items[&id(review)].findings[0];
    derived_id("fix", &id(review), off)
}

#[test]
fn only_a_final_review_gives_a_verdict_and_an_open_fix_blocks_a_merge() {
    let mut l = reviewed();
    l.agent("w2", claimed("v1", 1));
    l.agent("w2", verdict("v1", MergeVerdict::Merge));
    assert_eq!(last(&l), Some(Reject::NotReview));
    l.agent("w2", done("v1", 1, 0));
    l.agent("w3", claimed("v2", 1));
    // The final reviewer finds a fix of its own: a merge waits for it.
    l.agent("w3", finding("v2", FindingVerdict::Fix));
    l.agent("w3", verdict("v2", MergeVerdict::Merge));
    assert_eq!(last(&l), Some(Reject::OpenChildren));
    l.agent("w3", verdict("v2", MergeVerdict::Block));
    let s = state(&l);
    assert_eq!(s.rejections.len(), 2);
    assert_eq!(
        s.items[&id("r1")].review_verdict.as_ref().unwrap().verdict,
        MergeVerdict::Block
    );
    assert!(!s.merge_ready(&id("r1")));
}

#[test]
fn the_final_review_waits_for_a_fix_the_first_pass_asked_for_and_then_can_merge() {
    let mut l = reviewed();
    l.agent("w2", claimed("v1", 1));
    l.agent("w2", finding("v1", FindingVerdict::Fix));
    l.agent("w2", done("v1", 1, 0));
    let fix = fix_of(&l, "v1").to_string();
    // The first pass is done, but the fix is open: nobody can take the final review.
    assert_eq!(state(&l).stage(&id("v2")), Some(Stage::Blocked));
    l.agent("w3", claimed("v2", 1));
    assert_eq!(last(&l), Some(Reject::NotReady));
    // The fix lands.
    let fix = fix.as_str();
    l.agent("w1", claimed(fix, 1));
    l.agent("w1", done(fix, 1, 0));
    assert_eq!(state(&l).stage(&id("v2")), Some(Stage::Ready));
    l.agent("w3", claimed("v2", 1));
    l.agent("w3", verdict("v2", MergeVerdict::Merge));
    l.agent("w3", done("v2", 1, 0));
    assert!(state(&l).merge_ready(&id("r1")));
}

#[test]
fn a_verdict_is_bounded_and_goes_stale_with_the_request() {
    let mut l = at_final();
    l.agent(
        "w3",
        WorkRecord::ReviewVerdict {
            item: id("v2"),
            attempt: 1,
            verdict: MergeVerdict::Merge,
            why: "x".repeat(MAX_VERDICT_WHY + 1),
        },
    );
    assert_eq!(last(&l), Some(Reject::TooLong));
    l.agent(
        "w3",
        WorkRecord::ReviewVerdict {
            item: id("v2"),
            attempt: 1,
            verdict: MergeVerdict::Merge,
            why: String::new(),
        },
    );
    assert_eq!(last(&l), Some(Reject::MissingField));

    l.agent("w3", verdict("v2", MergeVerdict::Merge));
    assert!(state(&l).merge_ready(&id("r1")));
    // The root now reports another request: the verdict no longer applies.
    l.person(WorkRecord::AttemptClosed {
        item: id("r1"),
        attempt: 1,
    });
    let mut s = state(&l);
    s.items.get_mut(&id("r1")).unwrap().pr = Some("GetFelix/felix#10".into());
    assert!(!s.merge_ready(&id("r1")));
}

#[test]
fn a_revert_check_is_evidence_on_a_decision() {
    let mut l = held_root();
    let with = |check: Option<RevertCheck>| WorkRecord::Decision {
        item: id("r1"),
        attempt: 1,
        phase: Phase::Verify,
        so_far: "fix in".into(),
        question: String::new(),
        options: vec![],
        chosen: String::new(),
        why: "test guards it".into(),
        commit: "abc".into(),
        evidence: vec![],
        revert_check: check,
    };
    let check = |failed, passed| {
        Some(RevertCheck {
            test: "reads_see_a_fresh_write".into(),
            failed_without_fix: failed,
            passed_with_fix: passed,
        })
    };
    let proven = l.agent("w1", with(check(true, true)));
    l.agent("w1", with(check(false, true)));
    l.agent("w1", with(None));
    assert_eq!(state(&l).rejections.len(), 0);
    let p = &state(&l).items[&id("r1")].paths[0];
    assert_eq!(p.decisions.len(), 3);
    // Only a test that failed without the fix and passed with it counts.
    assert_eq!(p.proven, vec![proven]);

    l.agent(
        "w1",
        with(Some(RevertCheck {
            test: "t".repeat(MAX_REVERT_TEST + 1),
            failed_without_fix: true,
            passed_with_fix: true,
        })),
    );
    assert_eq!(last(&l), Some(Reject::TooLong));
    l.agent(
        "w1",
        with(Some(RevertCheck {
            test: String::new(),
            failed_without_fix: true,
            passed_with_fix: true,
        })),
    );
    assert_eq!(last(&l), Some(Reject::MissingField));
}
