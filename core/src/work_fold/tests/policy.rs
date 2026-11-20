//! The triage acceptance policy (format 25): what the tracker may accept for
//! a rule, and how a person takes it back.

use super::*;

fn triaged(item: &str, mutate: impl FnOnce(&mut WorkRecord)) -> WorkRecord {
    let mut r = WorkRecord::Triaged {
        item: id(item),
        tier: 2,
        why: "a typo in the docs".into(),
        duplicate_of: None,
        unclear: None,
        depends_on: vec![],
        related: vec![],
        split: vec![],
        item_type: None,
        area: None,
        size: None,
        needs: None,
    };
    mutate(&mut r);
    r
}

fn by_rule(item: &str, rule: &str) -> WorkRecord {
    WorkRecord::Accepted {
        item: id(item),
        tier: None,
        triage_skipped: None,
        admitted_by: Some(rule.into()),
    }
}

/// r1 is imported with a triage item t1 that w2 held; `answer` is its answer.
/// The item is left with t1 still open unless `finish`.
fn triaged_root(answer: WorkRecord, finish: bool) -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 25 });
    l.tracker(imported("r1"));
    l.tracker(proposed("t1", Some("r1"), "triage", WorkKind::Triage));
    l.agent("w2", claimed("t1", 1));
    l.agent("w2", answer);
    if finish {
        l.agent("w2", done("t1", 1, 0));
    }
    l
}

fn plain() -> Log {
    triaged_root(triaged("r1", |_| {}), true)
}

#[test]
fn the_tracker_accepts_a_finished_plain_triage_for_a_rule() {
    let mut l = plain();
    l.tracker(by_rule("r1", "docs-only"));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    let it = &s.items[&id("r1")];
    assert_eq!(it.admitted_by.as_deref(), Some("docs-only"));
    assert!(it.accepted && !it.accepted_by_person);
    assert_eq!(it.tier, Some(2));
    assert_eq!(s.stage(&id("r1")), Some(Stage::Ready));
    // A snapshot keeps who accepted it.
    let back: State = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(
        back.items[&id("r1")].admitted_by.as_deref(),
        Some("docs-only")
    );
}

#[test]
fn a_policy_acceptance_is_stamped_format_25() {
    let min = WorkLogged::new(0, key("tracker"), Actor::Tracker, by_rule("r1", "x")).min_format();
    assert_eq!(min, 25);
}

#[test]
fn only_the_tracker_may_write_a_rule_acceptance() {
    for who in 0..2 {
        let mut l = plain();
        if who == 0 {
            l.person(by_rule("r1", "docs-only"));
        } else {
            l.agent("w2", by_rule("r1", "docs-only"));
        }
        let s = fold(&l.0);
        assert_eq!(s.rejections.last().unwrap().why, Reject::ActorNotAllowed);
        assert!(!s.items[&id("r1")].accepted);
    }
}

#[test]
fn a_rule_cannot_accept_a_triage_that_needs_a_person() {
    type Change = fn(&mut WorkRecord);
    let cases: [(&str, Change); 4] = [
        ("unclear", |r| {
            if let WorkRecord::Triaged { unclear, .. } = r {
                *unclear = Some(Unclear::Repo);
            }
        }),
        ("duplicate", |r| {
            if let WorkRecord::Triaged { duplicate_of, .. } = r {
                *duplicate_of = Some(id("r9"));
            }
        }),
        ("split", |r| {
            if let WorkRecord::Triaged { split, .. } = r {
                *split = vec!["half".into()];
            }
        }),
        ("dependency", |r| {
            if let WorkRecord::Triaged { depends_on, .. } = r {
                *depends_on = vec![id("r9")];
            }
        }),
    ];
    for (name, mutate) in cases {
        let mut l = Log::default();
        l.person(WorkRecord::FormatRaised { to: 25 });
        l.tracker(imported("r9"));
        l.tracker(imported("r1"));
        l.tracker(proposed("t1", Some("r1"), "triage", WorkKind::Triage));
        l.agent("w2", claimed("t1", 1));
        l.agent("w2", triaged("r1", mutate));
        l.agent("w2", done("t1", 1, 0));
        l.tracker(by_rule("r1", "docs-only"));
        let s = fold(&l.0);
        assert_eq!(
            s.rejections.last().map(|r| r.why),
            Some(Reject::NotAdmissible),
            "{name}"
        );
        assert!(!s.items[&id("r1")].accepted, "{name}");
    }
}

#[test]
fn a_rule_waits_for_triage_to_finish_and_for_a_triage_to_exist() {
    let mut l = triaged_root(triaged("r1", |_| {}), false);
    l.tracker(by_rule("r1", "docs-only"));
    assert_eq!(last_rejection(&l), Some(Reject::NotAdmissible));
    // No triage item at all, as in propose mode: a person decides.
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 25 });
    l.tracker(imported("r1"));
    l.person(triaged("r1", |_| {}));
    l.tracker(by_rule("r1", "docs-only"));
    assert_eq!(last_rejection(&l), Some(Reject::NotAdmissible));
}

#[test]
fn a_rule_leaves_an_item_a_person_touched_alone() {
    // A person's own accept first: the rule cannot replace it.
    let mut l = plain();
    l.person(accept("r1"));
    l.tracker(by_rule("r1", "docs-only"));
    assert_eq!(last_rejection(&l), Some(Reject::NotAdmissible));
    assert!(fold(&l.0).items[&id("r1")].accepted_by_person);
    // An edited proposal is the person's answer now.
    let mut l = plain();
    l.person(WorkRecord::Edited {
        item: id("r1"),
        title: None,
        tier: None,
        labels: None,
        milestone: None,
        item_type: None,
        area: None,
        size: None,
        needs: None,
        depends_on: Some(vec![]),
        split: None,
        proposal: Some(4),
    });
    l.tracker(by_rule("r1", "docs-only"));
    assert_eq!(last_rejection(&l), Some(Reject::NotAdmissible));
    // Closed.
    let mut l = plain();
    l.person(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Dismissed,
    });
    l.tracker(by_rule("r1", "docs-only"));
    assert_eq!(last_rejection(&l), Some(Reject::NotAdmissible));
    // Changed upstream since triage.
    let mut l = plain();
    l.tracker(WorkRecord::Updated {
        item: id("r1"),
        title: Some("new".into()),
    });
    l.tracker(by_rule("r1", "docs-only"));
    assert_eq!(last_rejection(&l), Some(Reject::NotAdmissible));
}

#[test]
fn a_person_s_accept_after_the_rule_wins_and_clears_the_rule() {
    let mut l = plain();
    l.tracker(by_rule("r1", "docs-only"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: Some(0),
        triage_skipped: None,
        admitted_by: None,
    });
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    let it = &s.items[&id("r1")];
    assert_eq!(it.admitted_by, None);
    assert!(it.accepted_by_person);
    assert_eq!(it.tier, Some(0));
}

#[test]
fn a_person_takes_back_a_rule_acceptance_and_the_rule_stays_off() {
    let mut l = plain();
    l.tracker(by_rule("r1", "docs-only"));
    l.person(WorkRecord::AdmissionUndone { item: id("r1") });
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    let it = &s.items[&id("r1")];
    assert!(!it.accepted && it.policy_held && it.admitted_by.is_none());
    assert!(
        it.proposal.is_some(),
        "the triage answer stays for the person"
    );
    assert_eq!(s.stage(&id("r1")), Some(Stage::Triage));
    // The rule does not fire again; the person can still accept or reject.
    l.tracker(by_rule("r1", "docs-only"));
    assert_eq!(last_rejection(&l), Some(Reject::NotAdmissible));
    l.person(accept("r1"));
    let s = fold(&l.0);
    assert_eq!(s.rejections.len(), 1);
    assert!(s.items[&id("r1")].accepted_by_person);
}

#[test]
fn only_a_person_takes_back_and_only_a_rule_acceptance_before_work_starts() {
    // The tracker and agents cannot.
    let mut l = plain();
    l.tracker(by_rule("r1", "docs-only"));
    l.tracker(WorkRecord::AdmissionUndone { item: id("r1") });
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    // Not an item a person accepted.
    let mut l = plain();
    l.person(accept("r1"));
    l.person(WorkRecord::AdmissionUndone { item: id("r1") });
    assert_eq!(last_rejection(&l), Some(Reject::NotAdmissible));
    // Not once an agent holds it.
    let mut l = plain();
    l.tracker(by_rule("r1", "docs-only"));
    l.agent("w1", claimed("r1", 1));
    l.person(WorkRecord::AdmissionUndone { item: id("r1") });
    assert_eq!(last_rejection(&l), Some(Reject::NotAdmissible));
    assert!(fold(&l.0).items[&id("r1")].accepted);
    // Not twice.
    let mut l = plain();
    l.tracker(by_rule("r1", "docs-only"));
    l.person(WorkRecord::AdmissionUndone { item: id("r1") });
    l.person(WorkRecord::AdmissionUndone { item: id("r1") });
    assert_eq!(last_rejection(&l), Some(Reject::NotAdmissible));
}

#[test]
fn a_reopened_item_waits_for_a_person_again() {
    let mut l = plain();
    l.tracker(by_rule("r1", "docs-only"));
    l.person(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Dismissed,
    });
    l.person(WorkRecord::Reopened { item: id("r1") });
    let it = &fold(&l.0).items[&id("r1")];
    assert!(!it.accepted && it.admitted_by.is_none() && it.policy_held);
    l.tracker(by_rule("r1", "docs-only"));
    assert_eq!(last_rejection(&l), Some(Reject::NotAdmissible));
}

/// r1 is accepted and held by w1 with an unstarted triage child t1 and augment child a1.
fn held_with_routine_children(format: u32) -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: format });
    l.tracker(imported("r1"));
    l.person(accept("r1"));
    l.tracker(proposed("t1", Some("r1"), "triage", WorkKind::Triage));
    l.tracker(proposed("a1", Some("r1"), "augment", WorkKind::Augment));
    l.agent("w1", claimed("r1", 1));
    l
}

#[test]
fn finishing_a_fix_cancels_the_triage_and_augment_items_nobody_started() {
    let mut l = held_with_routine_children(25);
    l.agent("w1", done("r1", 1, 0));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    assert_eq!(s.items[&id("t1")].closed, Some(Closure::Cancelled));
    assert_eq!(s.items[&id("a1")].closed, Some(Closure::Cancelled));
    assert!(s.items[&id("r1")].paths[0].done);
}

#[test]
fn a_held_or_worked_child_still_keeps_the_parent_from_finishing() {
    // Held by another agent.
    let mut l = held_with_routine_children(25);
    l.agent("w2", claimed("t1", 1));
    l.agent("w1", done("r1", 1, 0));
    assert_eq!(last_rejection(&l), Some(Reject::OpenChildren));
    // Released, but it recorded work.
    l.agent(
        "w2",
        WorkRecord::Released {
            item: id("t1"),
            attempt: 1,
            reason: None,
            why: None,
            cost_micros: 0,
            usage: Vec::new(),
            floor_micros: 0,
        },
    );
    l.agent("w1", done("r1", 1, 0));
    assert_eq!(last_rejection(&l), Some(Reject::OpenChildren));
    let s = fold(&l.0);
    assert!(s.items[&id("t1")].closed.is_none() && s.items[&id("a1")].closed.is_none());
    // Another kind of child blocks too.
    let mut l = held_with_routine_children(25);
    l.tracker(proposed("f1", Some("r1"), "fix2", WorkKind::Fix));
    l.agent("w1", done("r1", 1, 0));
    assert_eq!(last_rejection(&l), Some(Reject::OpenChildren));
}

#[test]
fn below_format_25_open_triage_children_still_block_the_finish() {
    let mut l = held_with_routine_children(24);
    l.agent("w1", done("r1", 1, 0));
    assert_eq!(last_rejection(&l), Some(Reject::OpenChildren));
}
