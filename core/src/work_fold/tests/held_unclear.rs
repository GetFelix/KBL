//! An unclear triage answer on an item an agent already holds (format 27).

use super::*;

fn unclear(item: &str) -> WorkRecord {
    WorkRecord::Triaged {
        item: id(item),
        tier: 2,
        why: "the report does not say which repo".into(),
        duplicate_of: None,
        unclear: Some(Unclear::Repo),
        depends_on: vec![],
        related: vec![],
        split: vec![],
        item_type: None,
        area: None,
        size: None,
        needs: None,
    }
}

/// r1 is accepted before its triage item t1 finishes, and w1 claims the fix.
/// w2, which holds t1, then answers that the item is unclear.
fn log(format: u32) -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: format });
    l.tracker(imported("r1"));
    l.tracker(proposed("t1", Some("r1"), "triage", WorkKind::Triage));
    l.agent("w2", claimed("t1", 1));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: Some(1),
        triage_skipped: None,
        admitted_by: None,
    });
    l.agent("w1", claimed("r1", 1));
    l.agent("w2", unclear("r1"));
    l
}

#[test]
fn an_unclear_answer_frees_the_agent_that_holds_the_item_it_opens_a_question_on() {
    let s = fold(&log(27).0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    let r1 = &s.items[&id("r1")];
    assert!(r1.open_question.is_some());
    assert!(r1.hold.is_none(), "a question frees its asker");
    assert_eq!(s.stage(&id("r1")), Some(Stage::NeedsYou));
    // The claim w1 used is given back, as a question does.
    assert_eq!(r1.paths[0].tries, 0);
}

#[test]
fn below_format_27_the_holder_keeps_the_item_as_before() {
    let s = fold(&log(26).0);
    let r1 = &s.items[&id("r1")];
    assert!(r1.open_question.is_some());
    assert_eq!(r1.hold.as_ref().map(|h| h.by.as_str()), Some("w1"));
}

#[test]
fn an_unclear_answer_on_an_item_nobody_holds_only_opens_the_question() {
    let mut l = log(27);
    // Without w1's claim: the item is accepted and free.
    l.0.remove(5);
    let s = fold(&l.0);
    let r1 = &s.items[&id("r1")];
    assert!(r1.open_question.is_some() && r1.hold.is_none());
    assert!(
        r1.paths.is_empty(),
        "no claim was made, so none is given back"
    );
}
