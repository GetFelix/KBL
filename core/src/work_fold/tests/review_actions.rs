//! What a person does about a pull request and the agent's summary, from format 11.

use super::*;

const PR: &str = "GetFelix/felix#9";

/// A log raised to format 11 where `r1` was fixed by w1, who reported `PR`.
fn raised() -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 11 });
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: Some(1),
        triage_skipped: None,
    });
    l.agent("w1", claimed("r1", 1));
    l.agent(
        "w1",
        WorkRecord::Progress {
            item: id("r1"),
            attempt: 1,
            note: text("opened"),
            branch: None,
            pr: Some(PR.into()),
        },
    );
    l.agent("w1", done("r1", 1, 0));
    l
}

/// `raised` with upstream showing the request open and unreviewed.
fn open() -> Log {
    let mut l = raised();
    status(&mut l, PrChecks::Green, PrReview::Unreviewed);
    l
}

fn status(l: &mut Log, checks: PrChecks, review: PrReview) {
    l.tracker(pr_status("r1", snapshot(PR, checks, review)));
}

fn phase(l: &mut Log, phase: PrPhase) {
    let mut s = snapshot(PR, PrChecks::Green, PrReview::Unreviewed);
    s.phase = phase;
    l.tracker(pr_status("r1", s));
}

fn last_reject(l: &Log) -> Option<Reject> {
    fold(&l.0).rejections.last().map(|r| r.why)
}

fn ask(who: &str) -> WorkRecord {
    WorkRecord::ReviewRequested {
        item: id("r1"),
        reviewer: who.into(),
    }
}

fn changes() -> WorkRecord {
    WorkRecord::ChangesRequested { item: id("r1") }
}

fn merged() -> WorkRecord {
    WorkRecord::MergeMarked { item: id("r1") }
}

fn rated(attempt: u32, accurate: bool) -> WorkRecord {
    WorkRecord::SummaryRated {
        item: id("r1"),
        attempt,
        accurate,
    }
}

fn item(l: &Log) -> Item {
    fold(&l.0).items[&id("r1")].clone()
}

#[test]
fn a_review_request_records_the_ask_on_an_open_request() {
    let mut l = open();
    l.person(ask("kim"));
    let a = item(&l).review_ask.unwrap();
    assert_eq!((a.reviewer.as_str(), a.pr.as_str()), ("kim", PR));
    // The same ask again changes nothing; another reviewer replaces it.
    l.person(ask("kim"));
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    l.person(ask("ana"));
    assert_eq!(item(&l).review_ask.unwrap().reviewer, "ana");
    // Upstream's own view of the review is not touched.
    assert_eq!(
        fold(&l.0).items[&id("r1")]
            .pr_status
            .as_ref()
            .unwrap()
            .review,
        PrReview::Unreviewed
    );
}

#[test]
fn a_review_request_needs_an_open_unapproved_request_and_a_real_name() {
    // Upstream has not reported the request yet.
    let mut l = raised();
    l.person(ask("kim"));
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    let mut l = open();
    l.person(ask(""));
    assert_eq!(last_reject(&l), Some(Reject::MissingField));
    l.person(ask("ki\nm"));
    assert_eq!(last_reject(&l), Some(Reject::MissingField));
    l.person(ask(&"k".repeat(101)));
    assert_eq!(last_reject(&l), Some(Reject::TooLong));
    l.agent("w1", ask("kim"));
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    assert!(item(&l).review_ask.is_none());
    // Approved, merged and closed requests take no ask.
    status(&mut l, PrChecks::Green, PrReview::Approved);
    l.person(ask("kim"));
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    let mut l = open();
    phase(&mut l, PrPhase::Merged);
    l.person(ask("kim"));
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    let mut l = open();
    l.person(WorkRecord::Closed {
        item: id("r1"),
        why: CloseWhy::Dismissed,
    });
    l.person(ask("kim"));
    assert_eq!(last_reject(&l), Some(Reject::Closed));
}

#[test]
fn an_ask_lasts_until_upstream_answers_or_the_request_ends() {
    let mut l = open();
    l.person(ask("kim"));
    // A status that changes nothing about the review keeps it.
    status(&mut l, PrChecks::Pending, PrReview::Requested);
    assert!(item(&l).review_ask.is_some());
    status(&mut l, PrChecks::Green, PrReview::ChangesRequested);
    assert!(item(&l).review_ask.is_none());
    l.person(ask("kim"));
    assert!(item(&l).review_ask.is_some());
    phase(&mut l, PrPhase::Closed);
    assert!(item(&l).review_ask.is_none());
}

#[test]
fn a_repeated_changes_requested_status_keeps_an_ask_made_on_it() {
    let mut l = open();
    status(&mut l, PrChecks::Green, PrReview::ChangesRequested);
    l.person(ask("kim"));
    assert!(item(&l).review_ask.is_some());
    // The same review state again is not an answer.
    status(&mut l, PrChecks::Pending, PrReview::ChangesRequested);
    assert!(item(&l).review_ask.is_some());
    // A change to approved is.
    status(&mut l, PrChecks::Green, PrReview::Approved);
    assert!(item(&l).review_ask.is_none());
}

#[test]
fn a_changes_request_is_recorded_once_and_answers_the_ask() {
    let mut l = open();
    l.person(ask("kim"));
    l.person(changes());
    let it = item(&l);
    assert_eq!(it.changes_ask.as_ref().unwrap().pr, PR);
    assert!(it.review_ask.is_none());
    l.person(changes());
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    // A new review round starts over.
    l.person(ask("ana"));
    let it = item(&l);
    assert!(it.changes_ask.is_none() && it.review_ask.is_some());
    l.agent("w1", changes());
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
}

#[test]
fn a_changes_request_needs_an_open_unapproved_request() {
    let mut l = raised();
    l.person(changes());
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    let mut l = open();
    status(&mut l, PrChecks::Green, PrReview::Approved);
    l.person(changes());
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    let mut l = open();
    l.person(changes());
    // Upstream then approves, or the request ends: the ask is stale.
    status(&mut l, PrChecks::Green, PrReview::Approved);
    assert!(item(&l).changes_ask.is_none());
    let mut l = open();
    l.person(changes());
    phase(&mut l, PrPhase::Merged);
    assert!(item(&l).changes_ask.is_none());
}

#[test]
fn a_new_request_drops_the_asks() {
    let progress = |pr: &str| WorkRecord::Progress {
        item: id("r1"),
        attempt: 1,
        note: text("opened"),
        branch: None,
        pr: Some(pr.into()),
    };
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 11 });
    accepted_root(&mut l, "r1");
    l.agent("w1", claimed("r1", 1));
    l.agent("w1", progress(PR));
    status(&mut l, PrChecks::Green, PrReview::Unreviewed);
    l.person(ask("kim"));
    l.person(changes());
    l.person(ask("ana"));
    assert!(item(&l).review_ask.is_some());
    l.agent("w1", progress("GetFelix/felix#10"));
    assert_eq!(fold(&l.0).rejections, vec![]);
    let it = item(&l);
    assert!(it.review_ask.is_none() && it.changes_ask.is_none());
}

#[test]
fn a_new_attempt_on_the_same_request_drops_the_asks() {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 11 });
    accepted_root(&mut l, "r1");
    l.agent("w1", claimed("r1", 1));
    let d = l.agent("w1", decision("r1", 1, vec![]));
    l.agent(
        "w1",
        WorkRecord::Progress {
            item: id("r1"),
            attempt: 1,
            note: text("opened"),
            branch: None,
            pr: Some(PR.into()),
        },
    );
    l.agent("w1", done("r1", 1, 0));
    status(&mut l, PrChecks::Green, PrReview::Unreviewed);
    l.person(ask("kim"));
    l.person(changes());
    l.person(ask("ana"));
    l.person(WorkRecord::ForkedFrom {
        item: id("r1"),
        from_attempt: 1,
        at: d,
        note: None,
    });
    l.agent("w2", claimed("r1", 2));
    assert_eq!(fold(&l.0).rejections, vec![]);
    let it = item(&l);
    assert_eq!(it.pr.as_deref(), Some(PR));
    assert!(it.review_ask.is_none() && it.changes_ask.is_none());
}

#[test]
fn a_recorded_merge_closes_the_item_as_merged() {
    // Upstream has not said yet: the person saw it merge.
    let mut l = raised();
    l.agent("w1", merged());
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    l.person(merged());
    let it = item(&l);
    assert_eq!(it.closed, Some(Closure::Merged));
    let mark = it.merge_marked.unwrap();
    assert_eq!(mark.by, key("gabriel"));
    assert_eq!(mark.offset, (l.0.len() - 1) as u64);
    l.person(merged());
    assert_eq!(last_reject(&l), Some(Reject::Closed));
    // Upstream already says merged, and nobody closed the item yet.
    let mut l = raised();
    phase(&mut l, PrPhase::Merged);
    assert_eq!(item(&l).closed, None);
    l.person(merged());
    assert_eq!(item(&l).closed, Some(Closure::Merged));
}

#[test]
fn a_recorded_merge_is_refused_while_upstream_shows_the_request_open() {
    let mut l = open();
    l.person(merged());
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    let it = item(&l);
    assert!(it.closed.is_none() && it.merge_marked.is_none());
}

#[test]
fn a_later_open_status_sends_a_recorded_merge_back_to_needs_you() {
    for p in [PrPhase::Open, PrPhase::Closed] {
        let mut l = raised();
        l.person(merged());
        assert!(item(&l).merge_marked.is_some());
        phase(&mut l, p);
        let st = fold(&l.0);
        assert_eq!(st.rejections, vec![], "{p:?}");
        let it = &st.items[&id("r1")];
        assert!(it.closed.is_none() && it.merge_marked.is_some());
        assert_eq!(st.needs_you(&id("r1")), Some(NeedsYou::MergeDisputed));
        // Upstream merging after all settles it.
        let mut l2 = l;
        phase(&mut l2, PrPhase::Merged);
        let st = fold(&l2.0);
        assert!(st.items[&id("r1")].merge_marked.is_none());
        assert_ne!(st.needs_you(&id("r1")), Some(NeedsYou::MergeDisputed));
    }
    // A status that agrees leaves the closed item alone.
    let mut l = raised();
    l.person(merged());
    phase(&mut l, PrPhase::Merged);
    assert_eq!(item(&l).closed, Some(Closure::Merged));
}

#[test]
fn a_recorded_merge_is_refused_for_a_request_upstream_closed_or_none() {
    let mut l = raised();
    phase(&mut l, PrPhase::Closed);
    l.person(merged());
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    assert_eq!(item(&l).closed, None);
    // An item that reports no request has none to have merged.
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 11 });
    accepted_root(&mut l, "r1");
    l.person(merged());
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
}

#[test]
fn a_summary_mark_is_per_attempt_and_the_latest_stands() {
    let mut l = raised();
    assert_eq!(item(&l).paths[0].summary_accurate, None);
    l.person(rated(1, true));
    assert_eq!(item(&l).paths[0].summary_accurate, Some(true));
    l.person(rated(1, true));
    assert_eq!(last_reject(&l), Some(Reject::NoChange));
    l.person(rated(1, false));
    assert_eq!(item(&l).paths[0].summary_accurate, Some(false));
    l.agent("w1", rated(1, true));
    assert_eq!(last_reject(&l), Some(Reject::ActorNotAllowed));
    assert_eq!(item(&l).paths[0].summary_accurate, Some(false));
}

#[test]
fn only_a_finished_attempt_has_a_summary_to_mark() {
    let mut l = raised();
    l.person(rated(2, true));
    assert_eq!(last_reject(&l), Some(Reject::WrongAttempt));
    // An attempt still running has handed nothing off.
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 11 });
    accepted_root(&mut l, "r1");
    l.agent("w1", claimed("r1", 1));
    l.person(rated(1, true));
    assert_eq!(last_reject(&l), Some(Reject::NotReady));
}

#[test]
fn a_closed_items_summary_can_still_be_marked() {
    let mut l = raised();
    l.person(merged());
    assert!(item(&l).closed.is_some());
    l.person(rated(1, false));
    assert_eq!(item(&l).paths[0].summary_accurate, Some(false));
}

#[test]
fn a_rating_is_not_an_action_on_the_wait() {
    let mut l = raised();
    status(&mut l, PrChecks::Red, PrReview::Unreviewed);
    assert_eq!(fold(&l.0).needs_you(&id("r1")), Some(NeedsYou::CheckFailed));
    let before = fold(&l.0).time_to_human_action(0, u64::MAX);
    assert_eq!((before.open, before.all.count), (1, 0));
    l.person(rated(1, true));
    let after = fold(&l.0).time_to_human_action(0, u64::MAX);
    assert_eq!((after.open, after.all.count), (1, 0));
}

#[test]
fn the_new_kinds_are_refused_before_the_raise() {
    let mut l = Log::default();
    l.tracker(imported("r1"));
    for rec in [ask("kim"), changes(), merged(), rated(1, true)] {
        assert_eq!(rec.format(), 11, "{}", rec.kind());
        l.person(rec);
        assert_eq!(last_reject(&l), Some(Reject::Format));
    }
}
