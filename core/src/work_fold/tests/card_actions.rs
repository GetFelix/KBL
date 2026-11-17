//! The card actions a person takes on My queue, from format 10.

use super::*;

const HOUR: u64 = 3_600_000;

/// A log raised to format 10, with `r1` accepted.
fn raised() -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 10 });
    accepted_root(&mut l, "r1");
    l
}

fn last_reject(l: &Log) -> Option<Reject> {
    fold(&l.0).rejections.last().map(|r| r.why)
}

fn queue_ids(l: &Log, now_ms: u64) -> Vec<String> {
    fold(&l.0)
        .queue(now_ms)
        .iter()
        .map(|e| e.item.as_str().to_owned())
        .collect()
}

/// A raised log where `r1` is held, so its holder may propose.
fn held() -> Log {
    let mut l = raised();
    l.agent("w1", claimed("r1", 1));
    l.person(WorkRecord::BudgetSet {
        root: id("r1"),
        items_left: 500,
        max_depth: 3,
        cost_left_micros: 1_000_000,
        redos_per_item: 1,
    });
    l
}

/// `r1` released twice: the attempt used its tries.
fn tries_used() -> Log {
    let mut l = raised();
    for _ in 0..2 {
        l.agent("w1", claimed("r1", 1));
        l.agent("w1", release("r1", 1));
    }
    l
}

#[test]
fn a_retry_gives_an_item_that_used_its_tries_a_fresh_set() {
    let mut l = tries_used();
    let s = fold(&l.0);
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::TriesUsed));
    l.agent("w1", claimed("r1", 1));
    assert_eq!(last_reject(&l), Some(Reject::TooManyTries));
    l.person(WorkRecord::Retried { item: id("r1") });
    let s = fold(&l.0);
    assert_eq!(s.needs_you(&id("r1")), None);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Ready));
    l.agent("w1", claimed("r1", 1));
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::Held));
}

#[test]
fn a_retry_needs_a_person_and_an_item_that_stopped_for_its_tries() {
    let mut l = tries_used();
    l.agent("w1", WorkRecord::Retried { item: id("r1") });
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    // An item that is Ready has nothing to retry, and a retry is not repeated.
    let mut l = raised();
    l.person(WorkRecord::Retried { item: id("r1") });
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    let mut l = tries_used();
    l.person(WorkRecord::Retried { item: id("r1") });
    l.person(WorkRecord::Retried { item: id("r1") });
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
}

#[test]
fn a_snoozed_item_leaves_the_queue_and_returns() {
    let mut l = held();
    l.agent("w1", proposed("c1", None, "a", WorkKind::Fix));
    l.agent("w1", proposed("c2", None, "b", WorkKind::Fix));
    assert_eq!(queue_ids(&l, 0), ["c1", "c2"]);
    let at = l.0.last().unwrap().1.at_ms;
    l.person(WorkRecord::Snoozed {
        item: id("c1"),
        until_ms: at + 4 * HOUR,
    });
    assert_eq!(queue_ids(&l, at + HOUR), ["c2"]);
    // The time passes, and it is back in its place.
    assert_eq!(queue_ids(&l, at + 4 * HOUR), ["c1", "c2"]);
    // A snooze is not an action on the wait.
    let s = fold(&l.0).time_to_human_action(0, u64::MAX);
    assert_eq!((s.open, s.all.count), (2, 0));
}

#[test]
fn a_snooze_ends_when_the_item_waits_for_a_new_reason() {
    let mut l = raised();
    l.agent("w1", claimed("r1", 1));
    let q = l.agent("w1", question_rec("r1"));
    let at = l.0.last().unwrap().1.at_ms;
    l.person(WorkRecord::Snoozed {
        item: id("r1"),
        until_ms: at + 4 * HOUR,
    });
    assert!(queue_ids(&l, at + HOUR).is_empty());
    // Answered, then it fails and waits again: the snooze was for the question.
    l.person(answer_rec(q));
    l.agent("w1", release("r1", 1));
    l.agent("w1", claimed("r1", 1));
    l.agent("w1", release("r1", 1));
    assert_eq!(queue_ids(&l, at + HOUR), ["r1"]);
}

#[test]
fn a_bad_snooze_is_refused() {
    let mut l = held();
    l.agent("w1", proposed("c1", None, "a", WorkKind::Fix));
    let at = l.0.last().unwrap().1.at_ms;
    let snooze = |item: &str, until_ms| WorkRecord::Snoozed {
        item: id(item),
        until_ms,
    };
    l.person(snooze("c1", at));
    assert_eq!(last_reject(&l), Some(Reject::BadTime));
    l.person(snooze("c1", at + 31 * 24 * HOUR));
    assert_eq!(last_reject(&l), Some(Reject::BadTime));
    // r1 is Ready: it waits on nobody.
    l.person(snooze("r1", at + HOUR));
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    l.agent("w1", snooze("c1", at + HOUR));
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    assert!(fold(&l.0).items[&id("c1")].snoozed.is_none());
}

#[test]
fn a_parked_item_stays_out_until_unparked_or_it_waits_anew() {
    let mut l = held();
    l.agent("w1", proposed("c1", None, "a", WorkKind::Fix));
    let park = |parked| WorkRecord::Parked {
        item: id("c1"),
        parked,
    };
    l.person(park(true));
    assert!(queue_ids(&l, u64::MAX).is_empty());
    l.person(park(true));
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    l.person(park(false));
    assert_eq!(queue_ids(&l, 0), ["c1"]);
    l.person(park(false));
    assert_eq!(last_reject(&l), Some(Reject::NoChange));

    // Parked on its proposal; once accepted and failing it waits for a new reason.
    l.person(park(true));
    l.person(WorkRecord::Accepted {
        item: id("c1"),
        tier: None,
    });
    assert!(queue_ids(&l, 0).is_empty());
    for _ in 0..2 {
        l.agent("w1", claimed("c1", 1));
        l.agent("w1", release("c1", 1));
    }
    assert_eq!(queue_ids(&l, 0), ["c1"]);
}

#[test]
fn a_park_needs_an_item_that_waits() {
    let mut l = raised();
    l.person(WorkRecord::Parked {
        item: id("r1"),
        parked: true,
    });
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    l.agent(
        "w1",
        WorkRecord::Parked {
            item: id("r1"),
            parked: true,
        },
    );
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
}

#[test]
fn an_unblock_drops_the_link_that_holds_an_item() {
    let mut l = raised();
    accepted_root(&mut l, "r2");
    let unblock = || WorkRecord::Unblocked {
        item: id("r2"),
        to: id("r1"),
    };
    l.person(unblock());
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    l.tracker(WorkRecord::Linked {
        item: id("r2"),
        to: id("r1"),
    });
    assert_eq!(fold(&l.0).stage(&id("r2")), Some(Stage::Blocked));
    l.agent("w1", unblock());
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    l.person(unblock());
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("r2")), Some(Stage::Ready));
    assert!(!s.is_linked(&id("r2"), &id("r1")));
    l.person(unblock());
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
}

#[test]
fn the_new_kinds_are_refused_before_the_raise() {
    let mut l = Log::default();
    accepted_root(&mut l, "r1");
    for rec in [
        WorkRecord::Retried { item: id("r1") },
        WorkRecord::Snoozed {
            item: id("r1"),
            until_ms: 9,
        },
        WorkRecord::Parked {
            item: id("r1"),
            parked: true,
        },
        WorkRecord::Unblocked {
            item: id("r1"),
            to: id("r2"),
        },
    ] {
        assert_eq!(rec.format(), 10, "{}", rec.kind());
        l.person(rec);
        assert_eq!(last_reject(&l), Some(Reject::Format));
    }
}
