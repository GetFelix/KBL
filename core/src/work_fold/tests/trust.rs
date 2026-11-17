//! Trust in summaries: which marks count, and in which window.

use super::*;

/// A record's time is ten times its offset, so a window reads off the offsets.
fn trust(l: &Log) -> TrustStats {
    fold(&l.0).summary_trust(0, u64::MAX)
}

fn finished(l: &mut Log, item: &str) {
    accepted_root(l, item);
    l.agent("w1", claimed(item, 1));
    l.agent("w1", done(item, 1, 0));
}

fn rate(item: &str, accurate: bool) -> WorkRecord {
    WorkRecord::SummaryRated {
        item: id(item),
        attempt: 1,
        accurate,
    }
}

fn raised() -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 11 });
    l
}

#[test]
fn an_empty_log_has_no_share_to_report() {
    let s = trust(&Log::default());
    assert_eq!((s.rated, s.unrated, s.accurate_permille), (0, 0, None));
    assert_eq!(s.min_sample, MIN_SAMPLE);
}

#[test]
fn finished_attempts_nobody_marked_are_unrated_and_give_no_share() {
    let mut l = raised();
    finished(&mut l, "r1");
    finished(&mut l, "r2");
    // A running attempt has handed nothing off, so it is not waiting for a mark.
    accepted_root(&mut l, "r3");
    l.agent("w1", claimed("r3", 1));
    let s = trust(&l);
    assert_eq!((s.rated, s.unrated, s.accurate_permille), (0, 2, None));
}

#[test]
fn the_share_is_accurate_over_rated_and_unrated_stays_apart() {
    let mut l = raised();
    for r in ["r1", "r2", "r3", "r4"] {
        finished(&mut l, r);
    }
    l.person(rate("r1", true));
    l.person(rate("r2", true));
    l.person(rate("r3", false));
    let s = trust(&l);
    assert_eq!((s.rated, s.accurate, s.not_accurate), (3, 2, 1));
    assert_eq!((s.unrated, s.accurate_permille), (1, Some(666)));
}

#[test]
fn a_re_marked_attempt_counts_once_by_its_latest_mark_and_time() {
    let mut l = raised();
    finished(&mut l, "r1");
    let first = l.person(rate("r1", true));
    let second = l.person(rate("r1", false));
    let s = trust(&l);
    assert_eq!((s.rated, s.accurate, s.not_accurate), (1, 0, 1));
    let state = fold(&l.0);
    // The window holding only the first mark no longer has the attempt.
    let early = state.summary_trust(first * 10, second * 10);
    assert_eq!((early.rated, early.undated), (0, 0));
    let late = state.summary_trust(second * 10, u64::MAX);
    assert_eq!((late.rated, late.not_accurate), (1, 1));
    // A repeat of the standing mark is refused and moves nothing.
    l.person(rate("r1", false));
    let again = fold(&l.0).summary_trust(second * 10, u64::MAX);
    assert_eq!(again.rated, 1);
}

#[test]
fn a_mark_counts_in_the_window_it_was_written_in() {
    let mut l = raised();
    finished(&mut l, "r1");
    finished(&mut l, "r2");
    let a = l.person(rate("r1", true));
    let b = l.person(rate("r2", false));
    let state = fold(&l.0);
    let only_a = state.summary_trust(a * 10, b * 10);
    assert_eq!((only_a.rated, only_a.accurate), (1, 1));
    let only_b = state.summary_trust(b * 10, b * 10 + 1);
    assert_eq!((only_b.rated, only_b.not_accurate), (1, 1));
    // The unrated count is as of now, so it does not follow the window.
    assert_eq!(state.summary_trust(0, 1).unrated, 0);
}

#[test]
fn an_agent_cannot_move_the_numbers() {
    let mut l = raised();
    finished(&mut l, "r1");
    l.agent("w1", rate("r1", true));
    assert_eq!(trust(&l).rated, 0);
}

#[test]
fn a_state_decoded_from_its_wire_form_reports_its_marks_as_undated() {
    let mut l = raised();
    finished(&mut l, "r1");
    l.person(rate("r1", true));
    let wire = serde_json::to_string(&fold(&l.0)).unwrap();
    let back: State = serde_json::from_str(&wire).unwrap();
    let s = back.summary_trust(0, u64::MAX);
    assert_eq!((s.rated, s.undated), (0, 1));
}
