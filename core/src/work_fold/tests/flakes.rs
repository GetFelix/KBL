//! The flake list and ingest's one re-run of a known flaky check (format 22).

use super::*;
use crate::work::FORMAT_V22;

fn flake_set(flake: &str, check: &str, pattern: Option<&str>) -> WorkRecord {
    WorkRecord::FlakeSet {
        flake: key(flake),
        check: check.into(),
        pattern: pattern.map(Into::into),
        issue: "GetFelix/felix#1".into(),
    }
}

fn retried(item: &str, flake: &str, passed: bool) -> WorkRecord {
    WorkRecord::FlakeRetried {
        item: id(item),
        flake: key(flake),
        check: "lint".into(),
        passed,
    }
}

fn raised() -> Log {
    let mut l = with_pr();
    l.person(WorkRecord::FormatRaised { to: FORMAT_V22 });
    l
}

#[test]
fn flake_records_need_format_22() {
    let mut l = with_pr();
    l.person(flake_set("lint-timeout", "lint", None));
    assert_eq!(last_rejection(&l), Some(Reject::Format));
}

#[test]
fn only_a_person_keeps_the_flake_list() {
    let mut l = raised();
    l.agent("w1", flake_set("lint-timeout", "lint", None));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    l.tracker(flake_set("lint-timeout", "lint", None));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    l.person(flake_set("lint-timeout", "lint", Some("")));
    assert_eq!(last_rejection(&l), Some(Reject::BadFlake));
    l.person(flake_set("lint-timeout", "lint", Some("timed out")));
    let s = fold(&l.0);
    let f = &s.flakes[&key("lint-timeout")];
    assert_eq!(
        (f.check.as_str(), f.pattern.as_deref()),
        ("lint", Some("timed out"))
    );
    l.person(WorkRecord::FlakeCleared {
        flake: key("lint-timeout"),
    });
    assert!(fold(&l.0).flakes.is_empty());
    l.person(WorkRecord::FlakeCleared {
        flake: key("lint-timeout"),
    });
    assert_eq!(last_rejection(&l), Some(Reject::BadFlake));
}

#[test]
fn a_rerun_counts_a_hit_and_a_green_one_is_recorded_on_the_item() {
    let mut l = raised();
    l.person(flake_set("lint-timeout", "lint", None));
    l.tracker(retried("r1", "lint-timeout", false));
    let s = fold(&l.0);
    assert_eq!(s.flakes[&key("lint-timeout")].hits, 1);
    assert_eq!(s.items[&id("r1")].flake_retried, None);
    let at = l.tracker(retried("r1", "lint-timeout", true));
    // Replacing the entry keeps its count.
    l.person(flake_set("lint-timeout", "lint", Some("timed out")));
    let s = fold(&l.0);
    assert_eq!(s.flakes[&key("lint-timeout")].hits, 2);
    let r = s.items[&id("r1")].flake_retried.clone().unwrap();
    assert_eq!(
        (r.flake, r.issue.as_str(), r.offset),
        (key("lint-timeout"), "GetFelix/felix#1", at)
    );
    // The re-run kept the item out of Needs you: no red status was written.
    assert_eq!(s.stage(&id("r1")), Some(Stage::InReview));
}

#[test]
fn a_rerun_names_a_listed_flake_and_never_comes_from_an_agent() {
    let mut l = raised();
    l.tracker(retried("r1", "lint-timeout", true));
    assert_eq!(last_rejection(&l), Some(Reject::BadFlake));
    l.person(flake_set("lint-timeout", "lint", None));
    l.agent("w1", retried("r1", "lint-timeout", true));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    assert_eq!(fold(&l.0).flakes[&key("lint-timeout")].hits, 0);
}
