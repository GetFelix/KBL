//! Time to first human action: when a wait starts, which record ends it.

use super::*;

/// A record's time is ten times its offset, so a wait reads off the offsets.
fn stats(l: &Log) -> HumanWaitStats {
    fold(&l.0).time_to_human_action(0, u64::MAX)
}

fn close(item: &str) -> WorkRecord {
    WorkRecord::Closed {
        item: id(item),
        why: CloseWhy::default(),
    }
}

#[test]
fn a_proposal_waits_until_a_person_accepts_it() {
    let mut l = held_root();
    l.agent("w1", proposed("c1", None, "a", WorkKind::Fix)); // 4: at 40
    assert_eq!(stats(&l).open, 1);
    assert_eq!(stats(&l).all.count, 0);
    l.person(WorkRecord::Accepted {
        item: id("c1"),
        tier: None,
    }); // 5: at 50
    let s = stats(&l);
    assert_eq!((s.open, s.all.count, s.all.p50_ms), (0, 1, Some(10)));
    assert_eq!(s.by_kind[&WaitKind::Proposal].count, 1);
}

#[test]
fn a_question_waits_until_a_person_answers() {
    let mut l = held_root();
    let q = l.agent("w1", question_rec("r1")); // 4: at 40
    l.tracker(WorkRecord::Unknown); // 5
    l.person(answer_rec(q)); // 6: at 60
    let s = stats(&l);
    assert_eq!(s.by_kind[&WaitKind::Question].p50_ms, Some(20));
    assert_eq!(s.all.count, 1);
}

#[test]
fn a_failed_check_on_a_pr_in_review_starts_a_new_wait_that_a_person_ends() {
    let mut l = with_pr(); // done at offset 4: in review from 40
    assert_eq!(stats(&l).open, 1);
    l.tracker(pr_status(
        "r1",
        snapshot("GetFelix/felix#9", PrChecks::Red, PrReview::Unreviewed),
    )); // 5: the item goes on waiting, from 40
    assert_eq!(stats(&l).oldest_open_since_ms, Some(50));
    l.person(close("r1")); // 6: at 60
    let s = stats(&l);
    assert_eq!((s.all.p50_ms, s.open), (Some(10), 0));
    assert_eq!(s.by_kind[&WaitKind::Failure].count, 1);
}

#[test]
fn a_failure_starts_a_wait_of_its_own() {
    let mut l = held_root();
    l.agent(
        "w1",
        WorkRecord::Progress {
            item: id("r1"),
            attempt: 1,
            note: text("opened"),
            branch: None,
            pr: Some("GetFelix/felix#9".into()),
        },
    );
    l.agent(
        "w1",
        WorkRecord::Released {
            item: id("r1"),
            attempt: 1,
            reason: None,
            cost_micros: 0,
            usage: Vec::new(),
            floor_micros: 0,
        },
    ); // 5
    l.tracker(pr_status(
        "r1",
        snapshot("GetFelix/felix#9", PrChecks::Red, PrReview::Unreviewed),
    )); // 6: at 60
    assert_eq!(fold(&l.0).needs_you(&id("r1")), Some(NeedsYou::CheckFailed));
    l.person(close("r1")); // 7: at 70
    let s = stats(&l);
    assert_eq!(s.by_kind[&WaitKind::Failure].p50_ms, Some(10));
}

#[test]
fn agent_and_tracker_records_and_an_unknown_kind_do_not_end_a_wait() {
    let mut l = held_root();
    l.agent("w1", question_rec("r1")); // 4: at 40
    l.agent(
        "w1",
        WorkRecord::Progress {
            item: id("r1"),
            attempt: 1,
            note: text("still going"),
            branch: None,
            pr: None,
        },
    );
    // A kind this build does not know is no action, even when a person wrote it.
    let line =
        br#"{"at_ms":70,"by":"gabriel","actor":"person","kind":"item_starred","item":"r1","stars":3}"#;
    let snooze = WorkLogged::decode(line).unwrap();
    assert_eq!(snooze.record, WorkRecord::Unknown);
    let offset = l.0.len() as u64;
    l.0.push((offset, snooze));
    let s = stats(&l);
    assert_eq!((s.all.count, s.open), (0, 1));
}

#[test]
fn an_item_that_waits_twice_counts_twice() {
    let mut l = held_root();
    let q = l.agent("w1", question_rec("r1")); // 4: at 40
    l.person(answer_rec(q)); // 5: at 50
    let q = l.agent("w1", question_rec("r1")); // 6: at 60
    l.person(answer_rec(q)); // 7: at 70
    let s = stats(&l);
    assert_eq!((s.all.count, s.all.p50_ms, s.open), (2, Some(10), 0));
}

#[test]
fn a_person_acting_twice_on_one_wait_counts_it_once() {
    let mut l = with_pr(); // in review from 40
    for tier in [1, 2] {
        l.person(WorkRecord::TierChanged {
            item: id("r1"),
            tier,
        });
    }
    let s = stats(&l);
    assert_eq!((s.all.count, s.open), (1, 0));
}

#[test]
fn open_waits_stay_out_of_the_percentiles_and_the_window_filters() {
    let mut l = held_root();
    let q = l.agent("w1", question_rec("r1")); // at 40
    l.person(answer_rec(q)); // at 50
    l.agent("w1", question_rec("r1")); // at 60, left open
    let s = fold(&l.0).time_to_human_action(0, 1000);
    assert_eq!(
        (s.all.count, s.open, s.oldest_open_since_ms),
        (1, 1, Some(60))
    );
    let s = fold(&l.0).time_to_human_action(51, 1000);
    assert_eq!((s.all.count, s.all.p50_ms), (0, None));
}

#[test]
fn percentiles_use_the_nearest_rank() {
    let k = super::super::human_wait::stats((1..=10).collect());
    assert_eq!((k.count, k.p50_ms, k.p90_ms), (10, Some(5), Some(9)));
}

#[test]
fn each_team_folds_its_own_log() {
    let mut a = held_root();
    let q = a.agent("w1", question_rec("r1"));
    a.person(answer_rec(q));
    let b = held_root();
    assert_eq!(stats(&a).all.count, 1);
    assert_eq!(stats(&b).all.count, 0);
}

#[test]
fn waits_dropped_past_the_cap_are_reported_as_a_partial_window() {
    let mut l = held_root();
    for _ in 0..4 {
        let q = l.agent("w1", question_rec("r1"));
        l.person(answer_rec(q));
    }
    let mut s = State::default();
    s.keep_waits(2);
    for (o, r) in &l.0 {
        s.apply(*o, r);
    }
    // Two waits are kept; the newest dropped one ended at 70.
    let all = s.time_to_human_action(0, u64::MAX);
    assert_eq!((all.all.count, all.truncated), (2, true));
    assert_eq!(all.complete_since_ms, Some(71));
    assert!(!s.time_to_human_action(71, u64::MAX).truncated);
    assert!(!fold(&l.0).time_to_human_action(0, u64::MAX).truncated);
}

#[test]
fn a_restored_state_equals_the_full_fold_but_reports_what_it_missed() {
    let mut l = held_root();
    let q = l.agent("w1", question_rec("r1"));
    let cut = l.0.len();
    l.person(answer_rec(q));
    let full = fold(&l.0);
    let mut restored: State =
        serde_json::from_slice(&serde_json::to_vec(&fold(&l.0[..cut])).unwrap()).unwrap();
    // Nothing applied since the snapshot: the empty figures are not complete.
    assert!(restored.time_to_human_action(0, u64::MAX).truncated);
    for (o, r) in &l.0[cut..] {
        restored.apply(*o, r);
    }
    assert_eq!(restored, full);
    // The wait that began before the snapshot was never seen opening.
    assert_eq!(restored.time_to_human_action(0, u64::MAX).all.count, 0);
    assert_eq!(full.time_to_human_action(0, u64::MAX).all.count, 1);
    let r = restored.time_to_human_action(0, u64::MAX);
    assert!(r.truncated);
    let from = r.complete_since_ms.unwrap();
    assert!(!restored.time_to_human_action(from, u64::MAX).truncated);
    assert!(!full.time_to_human_action(0, u64::MAX).truncated);
}

#[test]
fn tracking_a_record_does_not_scan_every_edge() {
    use std::time::{Duration, Instant};
    let run = |n: usize| {
        let mut s = fold(&held_root().0);
        for i in 0..n {
            s.edges
                .insert((id(&format!("e{i}")), id(&format!("e{}", i + 1))));
        }
        let rec = WorkLogged::new(
            5,
            key("tracker"),
            Actor::Tracker,
            WorkRecord::TierChanged {
                item: id("r1"),
                tier: 1,
            },
        );
        let t = Instant::now();
        for _ in 0..n {
            s.track_human_wait(&rec);
        }
        t.elapsed()
    };
    let _ = run(1000);
    let small = run(4000).max(Duration::from_millis(20));
    let large = run(16_000);
    let ratio = large.as_secs_f64() / small.as_secs_f64();
    assert!(
        ratio < 10.0,
        "{small:?} then {large:?} for 4x the records and edges"
    );
}

#[test]
fn the_queue_lists_waits_oldest_first_then_by_tier() {
    let mut l = held_root();
    l.agent("w1", proposed("c1", None, "a", WorkKind::Fix)); // 4: at 40
    l.agent("w1", proposed("c2", None, "b", WorkKind::Fix)); // 5: at 50
    let ids = |l: &Log| {
        let q = fold(&l.0).queue(0);
        q.iter()
            .map(|e| e.item.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    let q = fold(&l.0).queue(0);
    assert_eq!((q[0].item.as_str(), q[0].since_ms), ("c1", Some(40)));
    assert_eq!(q[0].kind, WaitKind::Proposal);
    assert_eq!(ids(&l), ["c1", "c2"]);
    l.person(WorkRecord::Accepted {
        item: id("c1"),
        tier: None,
    });
    assert_eq!(ids(&l), ["c2"]);
}

fn entry(item: &str, tier: Option<u8>, since_ms: Option<u64>) -> QueueEntry {
    QueueEntry {
        item: id(item),
        kind: WaitKind::Other,
        tier,
        since_ms,
        assignee: None,
    }
}

fn order(mut v: Vec<QueueEntry>) -> Vec<String> {
    super::super::human_wait::sort_queue(&mut v);
    v.iter().map(|e| e.item.as_str().to_owned()).collect()
}

#[test]
fn queue_order_breaks_ties_by_tier_then_id_and_puts_unknown_starts_first() {
    // Equal start: the lower tier first, an untiered item last.
    assert_eq!(
        order(vec![
            entry("a", None, Some(5)),
            entry("b", Some(2), Some(5)),
            entry("c", Some(1), Some(5)),
        ]),
        ["c", "b", "a"]
    );
    // Equal start and tier: id.
    assert_eq!(
        order(vec![
            entry("z", Some(1), Some(5)),
            entry("y", Some(1), Some(5))
        ]),
        ["y", "z"]
    );
    // A tier never beats age.
    assert_eq!(
        order(vec![
            entry("a", Some(1), Some(9)),
            entry("b", None, Some(3))
        ]),
        ["b", "a"]
    );
    // An unknown start began before the snapshot, so it is oldest, whatever its tier.
    assert_eq!(
        order(vec![
            entry("a", Some(1), Some(0)),
            entry("b", None, None),
            entry("c", Some(2), None),
        ]),
        ["c", "b", "a"]
    );
}

#[test]
fn a_restored_state_queues_its_unseen_waits_first() {
    let mut l = held_root();
    l.agent("w1", proposed("c1", None, "a", WorkKind::Fix));
    let cut = l.0.len();
    l.agent("w1", proposed("c2", None, "b", WorkKind::Fix));
    let mut restored: State =
        serde_json::from_slice(&serde_json::to_vec(&fold(&l.0[..cut])).unwrap()).unwrap();
    for (o, r) in &l.0[cut..] {
        restored.apply(*o, r);
    }
    let q: Vec<_> = restored
        .queue(0)
        .iter()
        .map(|e| (e.item.as_str().to_owned(), e.since_ms))
        .collect();
    assert_eq!(q, [("c1".to_owned(), None), ("c2".to_owned(), Some(50))]);
}
