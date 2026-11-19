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

fn red_on(check: &str) -> WorkRecord {
    let mut s = snapshot("GetFelix/felix#9", PrChecks::Red, PrReview::Unreviewed);
    s.failing = Some(FailedCheck {
        name: check.into(),
        url: format!("https://example.test/{check}"),
    });
    pr_status("r1", s)
}

fn green() -> WorkRecord {
    pr_status(
        "r1",
        snapshot("GetFelix/felix#9", PrChecks::Green, PrReview::Unreviewed),
    )
}

fn let_go(item: &ItemId) -> WorkRecord {
    WorkRecord::Released {
        item: item.clone(),
        attempt: 1,
        reason: Some("lapsed".into()),
        why: None,
        cost_micros: 0,
        usage: Vec::new(),
        floor_micros: 0,
    }
}

#[test]
fn a_held_fix_outlives_green_and_closes_when_its_agent_lets_go() {
    let mut l = raised_red();
    l.tracker(failed("abc", true));
    let fix = fixes(&fold(&l.0))[0].id.clone();
    l.agent("w2", claimed(fix.as_str(), 1));
    l.tracker(green());
    assert_eq!(fold(&l.0).items[&fix].closed, None);
    // Ingest writes no second green, so the release is what ends it.
    l.tracker(let_go(&fix));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    assert_eq!(s.items[&fix].closed, Some(Closure::Completed));
    assert!(!s.fix_check_open(&id("r1"), "GetFelix/felix#9", "lint"));
}

#[test]
fn a_held_fix_let_go_while_its_check_is_red_stays_open() {
    let mut l = raised_red();
    l.tracker(failed("abc", true));
    let fix = fixes(&fold(&l.0))[0].id.clone();
    l.agent("w2", claimed(fix.as_str(), 1));
    l.tracker(let_go(&fix));
    let s = fold(&l.0);
    assert_eq!(s.items[&fix].closed, None);
    assert_eq!(s.stage(&fix), Some(Stage::Ready));
}

#[test]
fn a_red_on_another_check_leaves_the_fix_open_and_its_try_unspent() {
    let mut l = raised_red();
    l.tracker(failed("abc", true));
    let fix = fixes(&fold(&l.0))[0].id.clone();
    // Only one failing check is named, so lint may still be red.
    l.tracker(red_on("test"));
    let s = fold(&l.0);
    assert_eq!(s.items[&fix].closed, None);
    assert!(s.fix_check_open(&id("r1"), "GetFelix/felix#9", "lint"));
    assert_eq!(s.fix_check_tries(&id("r1"), "GetFelix/felix#9", "lint"), 1);
}

#[test]
fn a_root_keeps_at_most_32_failures() {
    let mut l = raised_red();
    for n in 0..32 {
        l.tracker(failed(&format!("h{n}"), false));
    }
    assert!(fold(&l.0).rejections.is_empty());
    l.tracker(failed("h32", false));
    assert_eq!(last_rejection(&l), Some(Reject::LimitReached));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].check_failures.len(), 32);
    assert!(
        !s.items[&id("r1")]
            .context
            .contains_key(&key("check-failed-33"))
    );
}

#[test]
fn a_spent_budget_records_the_failure_and_opens_no_fix() {
    let mut l = raised_red();
    l.person(WorkRecord::BudgetSet {
        root: id("r1"),
        items_left: 0,
        max_depth: 3,
        cost_left_micros: 1_000_000,
        redos_per_item: 1,
    });
    l.tracker(failed("abc", true));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    assert!(fixes(&s).is_empty());
    assert_eq!(s.items[&id("r1")].check_failures.len(), 1);
    assert_eq!(s.items[&id("r1")].check_failures[0].fix, None);
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::CheckFailed));
    assert_eq!(s.fix_check_tries(&id("r1"), "GetFelix/felix#9", "lint"), 0);
}

#[test]
fn a_refused_failure_changes_nothing() {
    let mut l = raised_red();
    // An item already holds the id the fix would get.
    let taken = fix_check_id(&id("r1"), "GetFelix/felix#9", "lint", "abc");
    l.tracker(imported(taken.as_str()));
    let before = fold(&l.0);
    l.tracker(failed("abc", true));
    assert_eq!(last_rejection(&l), Some(Reject::Duplicate));
    let after = fold(&l.0);
    assert_eq!(after.budgets[&id("r1")], before.budgets[&id("r1")]);
    assert_eq!(
        after.items[&id("r1")].context,
        before.items[&id("r1")].context
    );
}
