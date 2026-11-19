//! A failed check that becomes `fix_check` work (format 23).

use super::*;
use crate::work::FORMAT_V23;

fn failed(head: &str, fix: bool) -> WorkRecord {
    WorkRecord::CheckFailed {
        item: id("r1"),
        pr: "GetFelix/felix#9".into(),
        check: "lint".into(),
        url: "https://example.test/lint".into(),
        head: head.into(),
        log: "error: unused import".into(),
        fix,
    }
}

fn red() -> WorkRecord {
    pr_status(
        "r1",
        snapshot("GetFelix/felix#9", PrChecks::Red, PrReview::Unreviewed),
    )
}

fn raised_red() -> Log {
    let mut l = with_pr();
    l.person(WorkRecord::FormatRaised { to: FORMAT_V23 });
    l.tracker(red());
    l
}

fn fixes(s: &State) -> Vec<&Item> {
    s.items[&id("r1")]
        .children
        .iter()
        .map(|c| &s.items[c])
        .filter(|c| c.work == WorkKind::FixCheck)
        .collect()
}

#[test]
fn a_failed_check_needs_format_23() {
    let mut l = with_pr();
    l.tracker(red());
    l.tracker(failed("abc", true));
    assert_eq!(last_rejection(&l), Some(Reject::Format));
}

#[test]
fn a_failed_check_with_fix_opens_claimable_work_and_leaves_needs_you() {
    let mut l = raised_red();
    assert_eq!(fold(&l.0).needs_you(&id("r1")), Some(NeedsYou::CheckFailed));
    l.tracker(failed("abc", true));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    let f = fixes(&s);
    assert_eq!(f.len(), 1);
    assert_eq!(s.stage(&f[0].id), Some(Stage::Ready));
    assert!(f[0].context.contains_key(&key("check_log")));
    assert_eq!(s.needs_you(&id("r1")), None);
    assert_eq!(s.fix_check_tries(&id("r1"), "GetFelix/felix#9", "lint"), 1);
    assert_eq!(s.items[&id("r1")].check_failures.len(), 1);
}

#[test]
fn the_same_failure_twice_is_one_record_and_one_child() {
    let mut l = raised_red();
    l.tracker(failed("abc", true));
    l.tracker(failed("abc", true));
    assert_eq!(last_rejection(&l), Some(Reject::Duplicate));
    l.tracker(failed("abc", false));
    assert_eq!(last_rejection(&l), Some(Reject::Duplicate));
    assert_eq!(fixes(&fold(&l.0)).len(), 1);
}

#[test]
fn without_fix_the_failure_is_evidence_and_the_item_waits_on_a_person() {
    let mut l = raised_red();
    l.tracker(failed("abc", false));
    let s = fold(&l.0);
    assert!(fixes(&s).is_empty());
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::CheckFailed));
    assert!(
        s.items[&id("r1")]
            .context
            .contains_key(&key("check-failed-1"))
    );
}

#[test]
fn a_failure_must_match_the_red_status_and_come_from_the_tracker() {
    let mut l = with_pr();
    l.person(WorkRecord::FormatRaised { to: FORMAT_V23 });
    l.tracker(failed("abc", true));
    assert_eq!(last_rejection(&l), Some(Reject::NotFailing));
    l.tracker(red());
    l.agent("w1", failed("abc", true));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    l.person(failed("abc", true));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
}

#[test]
fn green_checks_close_the_fix_nobody_holds() {
    let mut l = raised_red();
    l.tracker(failed("abc", true));
    l.tracker(pr_status(
        "r1",
        snapshot("GetFelix/felix#9", PrChecks::Green, PrReview::Unreviewed),
    ));
    let s = fold(&l.0);
    assert_eq!(fixes(&s)[0].closed, Some(Closure::Completed));
}

#[test]
fn a_fix_that_did_not_help_hands_the_red_to_a_person() {
    let mut l = raised_red();
    l.tracker(failed("abc", true));
    let fix = fixes(&fold(&l.0))[0].id.clone();
    l.agent("w2", claimed(fix.as_str(), 1));
    l.agent("w2", done(fix.as_str(), 1, 0));
    // The push fails the same check again: the tracker opens no second try.
    l.tracker(failed("def", false));
    let s = fold(&l.0);
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::CheckFailed));
    assert_eq!(s.fix_check_tries(&id("r1"), "GetFelix/felix#9", "lint"), 1);
    assert_eq!(s.items[&id("r1")].check_failures.len(), 2);
}
