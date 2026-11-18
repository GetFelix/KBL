//! The inbox order and a person's edit of triage's advice (format 17).

use super::*;

fn triaged(item: &str, tier: u8) -> WorkRecord {
    WorkRecord::Triaged {
        item: id(item),
        tier,
        why: "fits".into(),
        duplicate_of: None,
        unclear: None,
        depends_on: vec![id("r9")],
        split: vec!["half".into()],
        area: None,
        item_type: None,
        size: None,
    }
}

fn edit(item: &str, deps: Option<&[&str]>, split: Option<&[&str]>, at: Option<u64>) -> WorkRecord {
    WorkRecord::Edited {
        item: id(item),
        title: None,
        tier: None,
        depends_on: deps.map(|d| d.iter().map(|x| id(x)).collect()),
        split: split.map(|d| d.iter().map(|x| (*x).to_owned()).collect()),
        proposal: at,
        area: None,
        item_type: None,
        labels: None,
        milestone: None,
        size: None,
    }
}

/// r1 holds triage's answer at the returned offset; r9 exists to be named.
fn triaged_root() -> (Log, u64) {
    let mut l = Log::default();
    l.tracker(imported("r9"));
    l.tracker(imported("r1"));
    l.person(WorkRecord::FormatRaised { to: 17 });
    let at = l.person(triaged("r1", 1));
    (l, at)
}

fn since(l: &Log, item: &str) -> Option<u64> {
    fold(&l.0).items[&id(item)].waiting_since.map(|w| w.at)
}

#[test]
fn an_item_waits_from_the_record_that_put_it_in_the_inbox() {
    let mut l = Log::default();
    l.tracker(imported("a"));
    l.tracker(imported("b"));
    assert_eq!(since(&l, "a"), None, "an import waits on nobody");
    let b = l.person(triaged("b", 1));
    let a = l.person(triaged("a", 1));
    assert_eq!(since(&l, "b"), Some(b));
    assert_eq!(since(&l, "a"), Some(a));
    // Triaging again for the same reason does not restart the wait.
    l.person(triaged("b", 0));
    assert_eq!(since(&l, "b"), Some(b));
    // Accepting ends the wait.
    l.person(WorkRecord::Accepted {
        item: id("b"),
        tier: None,
        triage_skipped: None,
    });
    assert_eq!(since(&l, "b"), None);
}

#[test]
fn a_new_reason_starts_a_new_wait_and_the_start_survives_a_snapshot_round_trip() {
    let mut l = held_root();
    let q = l.agent("w1", question_rec("r1"));
    assert_eq!(since(&l, "r1"), Some(q));
    let a = l.person(answer_rec(q));
    assert_eq!(since(&l, "r1"), None, "answered");
    l.agent("w1", claimed("r1", 1));
    let q2 = l.agent("w1", question_rec("r1"));
    assert!(q2 > a);
    let s = fold(&l.0);
    let back: State = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back, s);
    assert_eq!(back.items[&id("r1")].waiting_since.map(|w| w.at), Some(q2));
}

#[test]
fn a_person_adjusts_triage_s_dependencies_and_split() {
    let (mut l, at) = triaged_root();
    l.person(edit("r1", Some(&[]), Some(&["a", "b"]), Some(at)));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    let p = s.items[&id("r1")].proposal.clone().unwrap();
    assert!(p.depends_on.is_empty());
    assert_eq!(p.split, ["a", "b"]);
    assert_eq!(
        p.at, at,
        "the same answer, so a second edit may name it again"
    );
    l.person(edit("r1", Some(&["r9"]), None, Some(at)));
    let p = fold(&l.0).items[&id("r1")].proposal.clone().unwrap();
    assert_eq!(p.depends_on, [id("r9")]);
    assert_eq!(p.split, ["a", "b"], "a field left out stays");
}

#[test]
fn an_edit_of_advice_is_refused_unless_it_names_the_answer_it_read() {
    let (mut l, at) = triaged_root();
    l.person(edit("r1", Some(&[]), None, None));
    assert_eq!(last_rejection(&l), Some(Reject::StaleProposal));
    // Triage answers again: an edit made against the old answer must not land.
    let newer = l.person(triaged("r1", 2));
    l.person(edit("r1", None, Some(&["x"]), Some(at)));
    assert_eq!(last_rejection(&l), Some(Reject::StaleProposal));
    assert_eq!(
        fold(&l.0).items[&id("r1")].proposal.as_ref().unwrap().at,
        newer
    );
    // An item with no answer has nothing to edit.
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::FormatRaised { to: 17 });
    l.person(edit("r1", None, Some(&["x"]), Some(0)));
    assert_eq!(last_rejection(&l), Some(Reject::StaleProposal));
}

#[test]
fn dependencies_that_repeat_or_name_nothing_real_are_refused() {
    let (mut l, at) = triaged_root();
    for deps in [&["r9", "r9"][..], &["r1"], &["nope"]] {
        l.person(edit("r1", Some(deps), None, Some(at)));
        assert_eq!(last_rejection(&l), Some(Reject::BadDependency), "{deps:?}");
    }
    // Its own triage child is not another item either.
    l.tracker(proposed_triage());
    let child = fold(&l.0).items[&id("r1")].children[0].clone();
    l.person(edit("r1", Some(&[child.as_str()]), None, Some(at)));
    assert_eq!(last_rejection(&l), Some(Reject::BadDependency));
    assert_eq!(
        fold(&l.0).items[&id("r1")]
            .proposal
            .as_ref()
            .unwrap()
            .depends_on,
        [id("r9")]
    );
}

fn proposed_triage() -> WorkRecord {
    WorkRecord::Proposed {
        item: id("r1-triage"),
        parent: Some(id("r1")),
        key: key("triage"),
        work: WorkKind::Triage,
        title: "triage".into(),
        review: None,
    }
}

#[test]
fn only_a_person_adjusts_it_and_not_after_acceptance() {
    let (mut l, at) = triaged_root();
    l.agent("w1", edit("r1", None, Some(&["x"]), Some(at)));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
        triage_skipped: None,
    });
    l.person(edit("r1", None, Some(&["x"]), Some(at)));
    assert_eq!(last_rejection(&l), Some(Reject::NotAProposal));
}

#[test]
fn advice_in_an_edit_needs_format_17() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    l.person(WorkRecord::FormatRaised { to: 16 });
    let at = l.person(triaged("r1", 1));
    let rec = edit("r1", None, Some(&["x"]), Some(at));
    assert_eq!(rec.format(), 17);
    assert_eq!(edit("r1", None, None, None).format(), 1);
    l.person(rec);
    assert_eq!(last_rejection(&l), Some(Reject::Format));
}

#[test]
fn a_reopened_dependency_ends_the_wait_and_a_second_drop_is_a_new_one() {
    let mut l = Log::default();
    accepted_root(&mut l, "d");
    accepted_root(&mut l, "x");
    l.person(WorkRecord::Linked {
        item: id("d"),
        to: id("x"),
    });
    let close = |l: &mut Log| {
        l.person(WorkRecord::Closed {
            item: id("x"),
            why: CloseWhy::Dismissed,
        })
    };
    let first = close(&mut l);
    assert_eq!(since(&l, "d"), Some(first));
    l.person(WorkRecord::Reopened { item: id("x") });
    assert_eq!(since(&l, "d"), None);
    let second = close(&mut l);
    assert_eq!(since(&l, "d"), Some(second));
}

#[test]
fn a_root_going_over_budget_starts_a_new_wait_when_a_child_is_refused() {
    let mut l = Log::default();
    accepted_root(&mut l, "r1");
    l.person(WorkRecord::FormatRaised { to: 17 });
    l.person(proposed("c1", Some("r1"), "k", WorkKind::Test));
    let flagged = l.person(WorkRecord::Flagged {
        item: id("r1"),
        flagged: true,
    });
    assert_eq!(since(&l, "r1"), Some(flagged));
    let over = l.tracker(WorkRecord::Refused {
        item: Some(id("c1")),
        op: "claim".into(),
        reason: RefusalReason::Budget,
    });
    let s = fold(&l.0);
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::OverBudget));
    assert_eq!(s.items[&id("r1")].waiting_since.map(|w| w.at), Some(over));
}
