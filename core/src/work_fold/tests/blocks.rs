//! A person's block on an item, from format 16.

use super::*;

fn raised() -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 16 });
    accepted_root(&mut l, "r1");
    l
}

fn last_reject(l: &Log) -> Option<Reject> {
    fold(&l.0).rejections.last().map(|r| r.why)
}

fn block(item: &str, until_ms: Option<u64>, resource: Option<&str>) -> WorkRecord {
    WorkRecord::BlockSet {
        item: id(item),
        until_ms,
        resource: resource.map(str::to_owned),
        reason: "Azure credit is at zero".into(),
    }
}

#[test]
fn a_blocked_item_is_not_claimed_until_a_person_lifts_it() {
    let mut l = raised();
    l.person(block("r1", None, Some("azure-vms")));
    l.agent("w1", claimed("r1", 1));
    assert_eq!(last_reject(&l), Some(Reject::Blocked));
    let s = fold(&l.0);
    assert!(s.blocked_at(&id("r1"), u64::MAX));
    // Still Ready: the block is not a stage, so the card keeps its place.
    assert_eq!(s.stage(&id("r1")), Some(Stage::Ready));
    l.person(WorkRecord::BlockCleared { item: id("r1") });
    l.agent("w1", claimed("r1", 1));
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Held));
    assert!(s.items[&id("r1")].block.is_none());
}

#[test]
fn a_block_with_a_time_ends_by_itself_at_that_time() {
    let mut l = raised();
    // Offsets are ten milliseconds apart: the block is set at `at` and ends 25 later.
    let at = l.0.len() as u64 * 10;
    l.person(block("r1", Some(at + 25), None));
    l.agent("w1", claimed("r1", 1));
    assert_eq!(last_reject(&l), Some(Reject::Blocked));
    l.agent("w1", claimed("r1", 1));
    assert_eq!(last_reject(&l), Some(Reject::Blocked));
    l.agent("w1", claimed("r1", 1));
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Held));
    let b = s.items[&id("r1")].block.as_ref().expect("kept as set");
    assert!(b.holds_at(at + 24) && !b.holds_at(at + 25));
}

#[test]
fn a_holder_keeps_its_hold_when_the_item_is_blocked() {
    let mut l = raised();
    l.agent("w1", claimed("r1", 1));
    l.person(block("r1", None, Some("azure-vms")));
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Held));
    assert!(s.rejections.is_empty());
    // Once released it is Ready and blocked, so nobody is handed it.
    l.agent("w1", release("r1", 1));
    l.agent("w2", claimed("r1", 2));
    assert_eq!(last_reject(&l), Some(Reject::Blocked));
}

#[test]
fn a_block_needs_a_time_or_a_resource_a_plain_name_and_a_reason() {
    let mut l = raised();
    for bad in [
        block("r1", None, None),
        // In the past and too far out.
        block("r1", Some(10), Some("azure-vms")),
        block("r1", Some(u64::MAX / 2), None),
        block("r1", None, Some("two words")),
        block("r1", None, Some("")),
        WorkRecord::BlockSet {
            item: id("r1"),
            until_ms: None,
            resource: Some("azure-vms".into()),
            reason: "  ".into(),
        },
    ] {
        l.person(bad);
        assert_eq!(last_reject(&l), Some(Reject::BadBlock));
    }
    assert!(fold(&l.0).items[&id("r1")].block.is_none());
}

#[test]
fn only_a_person_sets_or_lifts_a_block_and_a_repeat_changes_nothing() {
    let mut l = raised();
    l.agent("w1", block("r1", None, Some("azure-vms")));
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    l.tracker(block("r1", None, Some("azure-vms")));
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    l.person(WorkRecord::BlockCleared { item: id("r1") });
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    l.person(block("r1", None, Some("azure-vms")));
    l.person(block("r1", None, Some("azure-vms")));
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    // A different reason or resource replaces it.
    l.person(block("r1", None, Some("gpu")));
    let s = fold(&l.0);
    assert_eq!(s.rejections.len(), 4);
    assert_eq!(
        s.items[&id("r1")]
            .block
            .as_ref()
            .unwrap()
            .resource
            .as_deref(),
        Some("gpu")
    );
}

#[test]
fn a_block_is_refused_below_format_16_and_a_reopen_drops_it() {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 15 });
    accepted_root(&mut l, "r1");
    l.person(block("r1", None, Some("azure-vms")));
    assert_eq!(last_reject(&l), Some(Reject::Format));

    let mut l = raised();
    l.person(block("r1", None, Some("azure-vms")));
    l.person(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Dismissed,
    });
    l.person(block("r1", None, Some("gpu")));
    assert_eq!(last_reject(&l), Some(Reject::Closed));
    l.person(WorkRecord::Reopened { item: id("r1") });
    assert!(fold(&l.0).items[&id("r1")].block.is_none());
}
