//! Folding to an offset: the state at every offset equals what a tracker
//! applying the log one record at a time held then, with or without snapshots.

use super::*;

/// The same records at offsets with blanks between and before them, as a
/// Raft shard leaves them. The citations inside the records are not moved,
/// so some are refused: the fold must treat that the same on every path.
fn with_blanks(log: &[(u64, WorkLogged)]) -> Vec<(u64, WorkLogged)> {
    log.iter()
        .enumerate()
        .map(|(i, (o, r))| (3 + o * 2 + (i as u64 % 3), r.clone()))
        .collect()
}

/// What a tracker held after each record: the one fold, recording as it goes.
fn held(log: &[(u64, WorkLogged)]) -> Vec<(u64, State)> {
    let mut s = State::default();
    log.iter()
        .map(|(o, r)| {
            s.apply(*o, r);
            (*o, s.clone())
        })
        .collect()
}

/// The state a tracker held at offset `n`: after the last record at or below it.
fn held_at(held: &[(u64, State)], n: u64) -> State {
    held.iter()
        .rev()
        .find(|(o, _)| *o <= n)
        .map_or_else(State::default, |(_, s)| s.clone())
}

/// What a state answers from the data `State`'s `PartialEq` ignores: the
/// pending counts and budget refusals per root, human-wait figures, the
/// queue and any trial in flight. Compared directly, since equality cannot.
fn derived(s: &State) -> String {
    let roots: Vec<_> = s.items.keys().cloned().collect();
    let exceed: Vec<_> = roots.iter().map(|r| s.proposals_exceed_budget(r)).collect();
    format!(
        "{:?}\n{:?}\n{exceed:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}",
        s.waiting,
        s.human_waits,
        s.time_to_human_action(0, u64::MAX),
        s.time_to_human_action(35, 75),
        s.queue(0),
        s.queue(HIDE_UNTIL / 2),
        s.queue(u64::MAX),
        s.probing,
        s.items
            .keys()
            .map(|r| (r.clone(), s.waiting.per_root.get(r).copied()))
            .collect::<Vec<_>>(),
    )
}

const HIDE_UNTIL: u64 = 3_600_000;

fn check_every_offset(log: &[(u64, WorkLogged)], every: u64) {
    let held = held(log);
    let snaps = Snapshots::build_every(log, every);
    let stored = Snapshots::from_stored(
        serde_json::from_value(serde_json::to_value(snaps.as_slice()).unwrap()).unwrap(),
    );
    let last = log.last().unwrap().0;
    for n in 0..=last + 2 {
        let want = held_at(&held, n);
        assert_eq!(fold_to(log, n), want, "fold_to({n})");
        assert_eq!(snaps.fold_to(log, n), want, "snapshots, at {n}");
        assert_eq!(stored.fold_to(log, n), want, "snapshots read back, at {n}");
        let d = derived(&want);
        assert_eq!(derived(&snaps.fold_to(log, n)), d, "derived, at {n}");
        assert_eq!(
            derived(&stored.fold_to(log, n)),
            d,
            "derived read back, at {n}"
        );
    }
}

#[test]
fn the_fold_at_every_offset_is_the_state_the_tracker_held() {
    let log = story().0;
    check_every_offset(&log, 1);
    check_every_offset(&log, 4);
    check_every_offset(&log, 100);
}

#[test]
fn blank_offsets_fold_to_the_state_before_them() {
    let log = with_blanks(&story().0);
    assert!(log.windows(2).any(|w| w[1].0 > w[0].0 + 1), "no blank");
    check_every_offset(&log, 1);
    check_every_offset(&log, 5);
    // Before the first record there is nothing; at a blank it is the last record's state.
    assert_eq!(fold_to(&log, log[0].0 - 1), State::default());
    let (o0, o1) = (log[0].0, log[1].0);
    assert!(o1 > o0 + 1);
    assert_eq!(fold_to(&log, o1 - 1).at, Some(o0));
}

#[test]
fn a_fold_is_the_same_every_time() {
    let log = with_blanks(&story().0);
    for n in [0, 9, 20, 1000] {
        assert_eq!(fold_to(&log, n), fold_to(&log, n));
    }
}

#[test]
fn a_stale_or_foreign_snapshot_is_not_trusted() {
    let log = story().0;
    let want = fold(&log);
    let good = Snapshots::build_every(&log, 4);
    assert!(good.as_slice().len() >= 3);

    let tamper = |f: &dyn Fn(&mut Snapshot)| {
        let mut snaps = good.as_slice().to_vec();
        snaps.iter_mut().for_each(f);
        Snapshots::from_stored(snaps)
    };
    let bad = [
        // A state that lies about where it ends.
        tamper(&|s| s.state.at = Some(s.through + 1)),
        // A chain that is not the log's.
        tamper(&|s| s.chain = "sha256-nope".into()),
        // Built by another fold.
        tamper(&|s| s.rev += 1),
        // A count that does not reach its record.
        tamper(&|s| s.records += 1),
        tamper(&|s| s.records = 0),
        // A state that is not the fold of this log.
        tamper(&|s| s.state = State::default()),
    ];
    for (i, snaps) in bad.iter().enumerate() {
        assert_eq!(snaps.fold_to(&log, u64::MAX), want, "bad snapshots {i}");
    }

    // Another log's snapshots do nothing for this one.
    let other = with_blanks(&log);
    let theirs = Snapshots::build_every(&other, 4);
    assert_eq!(theirs.fold_to(&log, u64::MAX), want);
    // A log shorter than the snapshots: trimmed, or rewritten below them.
    assert_eq!(good.fold_to(&log[..5], u64::MAX), fold(&log[..5]));
}

#[test]
fn refresh_rebuilds_a_stale_tail_and_extends_a_grown_log() {
    let log = story().0;
    let mut snaps = Snapshots::build_every(&log[..10], 4);
    // Corrupt the second snapshot: it and the ones after it go.
    let mut stored = snaps.as_slice().to_vec();
    stored[1].chain = "sha256-nope".into();
    snaps = Snapshots::from_stored(stored);
    snaps.refresh_every(&log, 4);
    assert_eq!(snaps, Snapshots::build_every(&log, 4));
    // A log that is not the one they came from rebuilds everything.
    let mut theirs = Snapshots::build_every(&with_blanks(&log), 4);
    theirs.refresh_every(&log, 4);
    assert_eq!(theirs, Snapshots::build_every(&log, 4));
}

#[test]
fn a_snapshot_is_made_every_two_thousand_records_whatever_the_blanks() {
    let log: Vec<_> = (0..4_100u64)
        .map(|i| {
            let rec = WorkLogged::new(
                i,
                key("tracker"),
                Actor::Tracker,
                imported(&format!("r{i}")),
            );
            // A blank after every fifth record.
            (i + i / 5, rec)
        })
        .collect();
    let snaps = Snapshots::build(&log);
    let at: Vec<_> = snaps.as_slice().iter().map(|s| s.records).collect();
    assert_eq!(at, vec![2_000, 4_000]);
    for n in [0, 2_499, 2_500, 2_501, 4_999, 5_000, u64::MAX] {
        assert_eq!(snaps.fold_to(&log, n), fold_to(&log, n), "at {n}");
    }
    // Asking exactly at a snapshot's offset lands on it.
    let first = snaps.as_slice()[0].through;
    assert_eq!(snaps.fold_to(&log, first).at, Some(first));
}

#[test]
fn a_snapshot_read_back_keeps_the_waiting_and_human_wait_data() {
    let mut l = held_root();
    l.person(WorkRecord::BudgetSet {
        root: id("r1"),
        items_left: 2,
        max_depth: 3,
        cost_left_micros: 1_000_000,
        redos_per_item: 3,
    });
    propose_n(&mut l, "w1", "a", 2);
    let q = l.agent("w1", question_rec("q1"));
    l.person(answer_rec(q));
    l.person(accept("a0"));
    propose_n(&mut l, "w1", "b", 1);
    let log = l.0;
    let end = fold(&log);
    // The log leaves a root at its limit, so the checks below are not empty.
    assert!(end.proposals_exceed_budget(&id("r1")));
    assert!(end.time_to_human_action(0, u64::MAX).all.count > 0);

    for every in [1, 2, 3] {
        let snaps = Snapshots::build_every(&log, every);
        let back: Vec<Snapshot> =
            serde_json::from_slice(&serde_json::to_vec(snaps.as_slice()).unwrap()).unwrap();
        assert!(back.iter().any(|s| !s.state().waiting.per_root.is_empty()));
        for read in &back {
            let want = fold_to(&log, read.through);
            assert_eq!(
                derived(&read.state()),
                derived(&want),
                "at {}",
                read.through
            );
        }
    }
}

#[test]
fn an_edit_to_an_earlier_record_fails_the_snapshot_that_covers_it() {
    let log = story().0;
    let snaps = Snapshots::build_every(&log, 4);
    assert!(snaps.as_slice().len() >= 3);
    // Same count, same offsets and same last record; only a record near the start differs.
    let mut edited = log.clone();
    edited[1].1 = WorkLogged::new(99, key("tracker"), Actor::Tracker, imported("elsewhere"));
    let want = fold(&edited);
    assert_ne!(want, fold(&log));
    assert_eq!(snaps.fold_to(&edited, u64::MAX), want);
    let mut refreshed = snaps.clone();
    refreshed.refresh_every(&edited, 4);
    assert_eq!(refreshed, Snapshots::build_every(&edited, 4));
}

/// A snooze and a park restored from a snapshot hide the item from the queue
/// and then show it again exactly as a fresh fold does.
#[test]
fn a_restored_park_or_snooze_folds_and_queues_like_a_fresh_one() {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: 10 });
    for c in ["c1", "c2", "c3"] {
        accepted_root(&mut l, c);
        for _ in 0..2 {
            l.agent("w1", claimed(c, 1));
            l.agent("w1", release(c, 1));
        }
    }
    l.person(WorkRecord::Parked {
        item: id("c1"),
        parked: true,
    });
    let at = l.0.last().unwrap().1.at_ms;
    l.person(WorkRecord::Snoozed {
        item: id("c2"),
        until_ms: at + HIDE_UNTIL,
    });
    let fresh = fold(&l.0);
    assert!(fresh.items[&id("c1")].parked_at_ms.is_some());
    assert!(fresh.items[&id("c2")].snoozed.is_some());
    let ids = |s: &State, now| -> Vec<String> {
        s.queue(now)
            .iter()
            .map(|e| e.item.as_str().to_owned())
            .collect()
    };
    assert_eq!(ids(&fresh, 0), ["c3"]);
    assert_eq!(ids(&fresh, u64::MAX), ["c2", "c3"]);
    check_every_offset(&l.0, 3);
    check_every_offset(&with_blanks(&l.0), 4);
}
