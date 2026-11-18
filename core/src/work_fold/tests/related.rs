//! A relation between items, from format 18: shown, never a hold.

use super::*;

fn raised() -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 18 });
    accepted_root(&mut l, "r1");
    accepted_root(&mut l, "r2");
    l
}

fn related(a: &str, b: &str) -> WorkRecord {
    WorkRecord::Related {
        item: id(a),
        to: id(b),
    }
}

fn rejected(l: &Log) -> Vec<Reject> {
    fold(&l.0).rejections.iter().map(|r| r.why).collect()
}

#[test]
fn a_related_item_is_ready_while_its_partner_is_open_and_a_dependency_is_not() {
    let mut l = raised();
    l.person(related("r1", "r2"));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty());
    assert_eq!(s.stage(&id("r1")), Some(Stage::Ready));
    assert_eq!(s.stage(&id("r2")), Some(Stage::Ready));
    assert!(s.blockers(&id("r1")).is_empty());

    let mut l = raised();
    l.person(WorkRecord::Linked {
        item: id("r1"),
        to: id("r2"),
    });
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Blocked));
    assert_eq!(s.stage(&id("r2")), Some(Stage::Ready));
}

#[test]
fn a_relation_goes_both_ways_and_unrelate_removes_both() {
    let mut l = raised();
    l.person(related("r1", "r2"));
    l.person(related("r2", "r1"));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty());
    assert_eq!(s.related.len(), 2, "one relation, stored once each way");
    assert_eq!(s.related_to(&id("r2")).collect::<Vec<_>>(), [&id("r1")]);
    l.person(WorkRecord::Unrelated {
        item: id("r2"),
        to: id("r1"),
    });
    assert!(fold(&l.0).related.is_empty());
}

#[test]
fn a_relation_may_close_a_loop_but_never_relates_an_item_to_itself() {
    let mut l = raised();
    accepted_root(&mut l, "r3");
    l.person(related("r1", "r2"));
    l.person(related("r2", "r3"));
    l.person(related("r3", "r1"));
    assert!(rejected(&l).is_empty());
    l.person(related("r1", "r1"));
    assert_eq!(rejected(&l), [Reject::Cycle]);
}

#[test]
fn only_a_person_relates_and_only_known_items() {
    let mut l = raised();
    l.agent("w1", related("r1", "r2"));
    l.tracker(related("r1", "r2"));
    l.agent(
        "w1",
        WorkRecord::Unrelated {
            item: id("r1"),
            to: id("r2"),
        },
    );
    l.person(related("r1", "nope"));
    assert_eq!(
        rejected(&l),
        [
            Reject::ActorNotAllowed,
            Reject::ActorNotAllowed,
            Reject::ActorNotAllowed,
            Reject::UnknownItem
        ]
    );
    assert!(fold(&l.0).related.is_empty());
}

#[test]
fn an_item_has_a_bounded_number_of_relations() {
    let mut l = raised();
    for i in 0..=MAX_RELATED {
        accepted_root(&mut l, &format!("x{i}"));
        l.person(related("r1", &format!("x{i}")));
    }
    let s = fold(&l.0);
    assert_eq!(s.related_to(&id("r1")).count(), MAX_RELATED);
    assert_eq!(rejected(&l), [Reject::LimitReached]);
}

#[test]
fn below_18_a_relation_is_refused() {
    let mut l = Log::default();
    accepted_root(&mut l, "r1");
    accepted_root(&mut l, "r2");
    l.person(related("r1", "r2"));
    l.person(WorkRecord::FormatRaised { to: 17 });
    l.person(related("r1", "r2"));
    assert_eq!(rejected(&l), [Reject::Format, Reject::Format]);
    assert!(fold(&l.0).related.is_empty());
}
