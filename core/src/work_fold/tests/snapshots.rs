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
        // A tail record that is not the log's.
        tamper(&|s| s.tail = "sha256-nope".into()),
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
    stored[1].tail = "sha256-nope".into();
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
