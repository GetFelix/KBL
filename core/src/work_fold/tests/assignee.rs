//! Who works an item, and a note sent to the agent holding it, from format 12.

use super::*;

/// A log raised to format 12, with `r1` accepted.
fn raised() -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 12 });
    accepted_root(&mut l, "r1");
    l
}

fn last_reject(l: &Log) -> Option<Reject> {
    fold(&l.0).rejections.last().map(|r| r.why)
}

fn taken() -> WorkRecord {
    WorkRecord::Taken { item: id("r1") }
}

fn to(who: Option<Assignee>) -> WorkRecord {
    WorkRecord::Reassigned {
        item: id("r1"),
        to: who,
    }
}

fn person(k: &str) -> Option<Assignee> {
    Some(Assignee::Person(key(k)))
}

fn steered(attempt: u32) -> WorkRecord {
    WorkRecord::Steered {
        item: id("r1"),
        attempt,
        text: text("use the writer lock"),
    }
}

fn released(attempt: u32) -> WorkRecord {
    WorkRecord::Released {
        item: id("r1"),
        attempt,
        reason: None,
        why: None,
        cost_micros: 0,
        usage: Vec::new(),
        floor_micros: 0,
    }
}

fn queue_of(l: &Log, viewer: Option<&str>) -> Vec<String> {
    let viewer = viewer.map(key);
    fold(&l.0)
        .queue_for(0, viewer.as_ref())
        .iter()
        .map(|e| e.item.as_str().to_owned())
        .collect()
}

#[test]
fn a_take_assigns_the_writer_and_keeps_the_item_out_of_ready() {
    let mut l = raised();
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::Ready));
    l.person(taken());
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].assignee, person("gabriel"));
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::Taken));
    assert_eq!(s.stage(&id("r1")), Some(Stage::NeedsYou));
    // No agent claims it.
    l.agent("w1", claimed("r1", 1));
    assert_eq!(last_reject(&l), Some(Reject::NotReady));
    assert!(fold(&l.0).items[&id("r1")].hold.is_none());
}

#[test]
fn a_take_is_refused_by_an_agent_on_a_closed_or_unaccepted_item_and_when_already_theirs() {
    let mut l = raised();
    l.agent("w1", taken());
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    l.person(taken());
    l.person(taken());
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 12 });
    l.tracker(imported("r1"));
    l.person(taken());
    assert_eq!(last_reject(&l), Some(Reject::NotReady));
    let mut l = raised();
    l.person(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Dismissed,
    });
    l.person(taken());
    assert_eq!(last_reject(&l), Some(Reject::Closed));
}

#[test]
fn a_take_needs_format_12() {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 11 });
    accepted_root(&mut l, "r1");
    l.person(taken());
    assert_eq!(last_reject(&l), Some(Reject::Format));
    assert_eq!(fold(&l.0).items[&id("r1")].assignee, None);
}

#[test]
fn a_reassign_sets_or_clears_the_assignee() {
    let mut l = raised();
    l.person(to(person("ana")));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].assignee, person("ana"));
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::Taken));
    l.person(to(person("ana")));
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    // Nobody: an agent may claim it again.
    l.person(to(None));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].assignee, None);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Ready));
    l.person(to(None));
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    l.agent("w1", claimed("r1", 1));
    assert!(fold(&l.0).items[&id("r1")].hold.is_some());
}

#[test]
fn a_reassign_to_an_agent_below_format_13_or_by_an_agent_is_refused() {
    let mut l = raised();
    l.person(to(Some(Assignee::Agent(key("w1")))));
    assert_eq!(last_reject(&l), Some(Reject::BadAssignee));
    l.agent("w1", to(person("ana")));
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    let mut l = raised();
    l.person(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Dismissed,
    });
    l.person(to(person("ana")));
    assert_eq!(last_reject(&l), Some(Reject::Closed));
    assert_eq!(fold(&l.0).items[&id("r1")].assignee, None);
}

#[test]
fn a_holder_keeps_its_hold_and_the_item_then_stops_for_the_assignee() {
    let mut l = raised();
    l.agent("w1", claimed("r1", 1));
    l.person(taken());
    let s = fold(&l.0);
    assert_eq!(s.stage(&id("r1")), Some(Stage::Held));
    assert_eq!(s.items[&id("r1")].hold.as_ref().unwrap().by, key("w1"));
    l.agent("w1", released(1));
    assert_eq!(fold(&l.0).rejections, vec![]);
    let s = fold(&l.0);
    assert_eq!(s.needs_you(&id("r1")), Some(NeedsYou::Taken));
    l.agent("w2", claimed("r1", 1));
    assert_eq!(last_reject(&l), Some(Reject::NotReady));
}

#[test]
fn other_waits_and_stages_outrank_a_take() {
    // Flagged says what to do; the assignee only says who.
    let mut l = raised();
    l.person(taken());
    l.person(WorkRecord::Flagged {
        item: id("r1"),
        flagged: true,
    });
    assert_eq!(fold(&l.0).needs_you(&id("r1")), Some(NeedsYou::Flagged));
    // Done work is in review, not waiting on the assignee.
    let mut l = raised();
    l.agent("w1", claimed("r1", 1));
    l.agent("w1", done("r1", 1, 0));
    l.person(taken());
    assert_eq!(fold(&l.0).stage(&id("r1")), Some(Stage::InReview));
}

#[test]
fn the_assignee_survives_close_and_reopen() {
    let mut l = raised();
    l.person(to(person("ana")));
    l.person(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Dismissed,
    });
    l.person(WorkRecord::Reopened { item: id("r1") });
    assert_eq!(fold(&l.0).items[&id("r1")].assignee, person("ana"));
}

#[test]
fn my_queue_shows_an_assigned_item_only_to_its_assignee() {
    let mut l = raised();
    accepted_root(&mut l, "r2");
    for item in ["r1", "r2"] {
        l.person(WorkRecord::Flagged {
            item: id(item),
            flagged: true,
        });
    }
    l.person(to(person("ana")));
    assert_eq!(queue_of(&l, Some("ana")), ["r1", "r2"]);
    assert_eq!(queue_of(&l, Some("kim")), ["r2"]);
    assert_eq!(queue_of(&l, None), ["r2"]);
    // The team view still lists both, with the assignee.
    let q = fold(&l.0).queue(0);
    assert_eq!(q.len(), 2);
    assert_eq!(q[0].assignee, person("ana"));
}

#[test]
fn a_take_ends_a_wait_and_the_wait_it_starts_counts_as_acted() {
    let mut l = raised();
    l.person(WorkRecord::Flagged {
        item: id("r1"),
        flagged: true,
    });
    l.person(WorkRecord::Flagged {
        item: id("r1"),
        flagged: false,
    });
    l.person(taken());
    let s = fold(&l.0);
    assert_eq!(queue_of(&l, Some("gabriel")), ["r1"]);
    // The person chose this wait, so it is not one waiting on a first look.
    assert_eq!(s.time_to_human_action(0, u64::MAX).open, 0);
}

#[test]
fn a_steer_reaches_the_held_attempt_like_a_note_on_it() {
    let mut l = raised();
    l.agent("w1", claimed("r1", 1));
    let at = l.person(steered(1));
    let s = fold(&l.0);
    assert_eq!(s.rejections, vec![]);
    assert_eq!(s.items[&id("r1")].paths[0].human_inputs, vec![at]);
    assert!(s.human_inputs_on_path(&id("r1"), 1).contains(&at));
    assert_eq!(s.items[&id("r1")].newest_note(), Some(at));
}

#[test]
fn a_steer_needs_a_holder_on_the_attempt_it_names() {
    let mut l = raised();
    l.person(steered(1));
    assert_eq!(last_reject(&l), Some(Reject::NotHeld));
    l.agent("w1", claimed("r1", 1));
    l.person(steered(2));
    assert_eq!(last_reject(&l), Some(Reject::WrongAttempt));
    l.agent("w1", steered(1));
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    l.agent("w1", released(1));
    l.person(steered(1));
    assert_eq!(last_reject(&l), Some(Reject::NotHeld));
    l.person(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Dismissed,
    });
    l.person(steered(1));
    assert_eq!(last_reject(&l), Some(Reject::Closed));
    assert!(fold(&l.0).items[&id("r1")].paths[0].human_inputs.is_empty());
}

#[test]
fn a_steer_on_an_earlier_attempt_is_refused_while_a_redo_is_held() {
    let mut l = raised();
    l.agent("w1", claimed("r1", 1));
    let d = l.agent("w1", decision("r1", 1, vec![]));
    l.agent("w1", done("r1", 1, 0));
    l.person(WorkRecord::ForkedFrom {
        item: id("r1"),
        from_attempt: 1,
        at: d,
        note: None,
    });
    l.agent("w2", claimed("r1", 2));
    l.person(steered(1));
    assert_eq!(last_reject(&l), Some(Reject::WrongAttempt));
    assert!(fold(&l.0).items[&id("r1")].paths[0].human_inputs.is_empty());
    let at = l.person(steered(2));
    assert_eq!(fold(&l.0).items[&id("r1")].paths[1].human_inputs, vec![at]);
}

fn agent(k: &str) -> Option<Assignee> {
    Some(Assignee::Agent(key(k)))
}

/// A log raised to format 13, with `r1` accepted.
fn raised_13() -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 13 });
    accepted_root(&mut l, "r1");
    l
}

#[test]
fn an_agent_assignee_is_the_only_agent_that_may_claim() {
    let mut l = raised_13();
    l.person(to(agent("w1")));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].assignee, agent("w1"));
    // Still Ready: an agent assignee does not wait on a person.
    assert_eq!(s.stage(&id("r1")), Some(Stage::Ready));
    assert_eq!(s.needs_you(&id("r1")), None);
    l.agent("w2", claimed("r1", 1));
    assert_eq!(last_reject(&l), Some(Reject::NotAssignee));
    assert_eq!(fold(&l.0).items[&id("r1")].hold, None);
    l.agent("w1", claimed("r1", 1));
    let s = fold(&l.0);
    assert_eq!(
        s.items[&id("r1")].hold.as_ref().map(|h| h.by.clone()),
        Some(key("w1"))
    );
    // An agent assignee's waiting item is in everyone's queue.
    l.agent("w1", released(1));
    l.person(WorkRecord::Flagged {
        item: id("r1"),
        flagged: true,
    });
    assert_eq!(queue_of(&l, Some("ana")), vec!["r1"]);
}

#[test]
fn a_reassign_while_held_never_makes_a_second_holder() {
    let mut l = raised_13();
    l.agent("w1", claimed("r1", 1));
    // Reassigned and taken while w1 holds: w1 keeps the one hold.
    l.person(to(agent("w2")));
    l.agent("w2", claimed("r1", 1));
    assert_eq!(last_reject(&l), Some(Reject::NotReady));
    l.agent("w2", claimed("r1", 2));
    assert_eq!(last_reject(&l), Some(Reject::NotReady));
    l.person(taken());
    l.person(to(agent("w2")));
    let s = fold(&l.0);
    assert_eq!(
        s.items[&id("r1")].hold.as_ref().map(|h| h.by.clone()),
        Some(key("w1"))
    );
    // Once it lets go, only the new assignee may take the next try.
    l.agent("w1", released(1));
    l.agent("w1", claimed("r1", 1));
    assert_eq!(last_reject(&l), Some(Reject::NotAssignee));
    l.agent("w2", claimed("r1", 1));
    let s = fold(&l.0);
    assert_eq!(
        s.items[&id("r1")].hold.as_ref().map(|h| h.by.clone()),
        Some(key("w2"))
    );
}

#[test]
fn the_tracker_may_only_drop_an_agent_assignee() {
    let mut l = raised_13();
    l.tracker(to(None));
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    l.person(to(person("ana")));
    l.tracker(to(None));
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    l.person(to(agent("w1")));
    l.tracker(to(agent("w2")));
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    l.tracker(to(None));
    let s = fold(&l.0);
    assert_eq!(s.items[&id("r1")].assignee, None);
    l.agent("w2", claimed("r1", 1));
    assert!(fold(&l.0).items[&id("r1")].hold.is_some());
}

#[test]
fn an_agent_assignee_needs_a_log_at_format_13() {
    // Stamped 12 like any reassign: the log's format decides, so a log
    // below 13 folds as it did before.
    assert_eq!(to(agent("w1")).format(), 12);
    let mut l = raised();
    l.person(to(agent("w1")));
    assert_eq!(last_reject(&l), Some(Reject::BadAssignee));
    l.agent("w2", claimed("r1", 1));
    assert!(fold(&l.0).items[&id("r1")].hold.is_some());
}

#[test]
fn the_tracker_cannot_drop_an_assignee_of_a_closed_item() {
    let mut l = raised_13();
    l.person(to(agent("w1")));
    l.person(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Dismissed,
    });
    l.tracker(to(None));
    assert_eq!(fold(&l.0).items[&id("r1")].assignee, agent("w1"));
    assert!(last_reject(&l).is_some());
}
