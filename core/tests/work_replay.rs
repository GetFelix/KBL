//! A log written before prerequisites existed must fold to the same states.

use agents_core::codec::Codec;
use agents_core::names::ItemId;
use agents_core::work::WorkLogged;
use agents_core::work_fold::{Stage, State};

fn log() -> Vec<(u64, WorkLogged)> {
    include_str!("work_pre_prerequisite.jsonl")
        .lines()
        .enumerate()
        .map(|(i, l)| (i as u64, WorkLogged::decode(l.as_bytes()).unwrap()))
        .collect()
}

fn stage(s: &State, id: &str) -> Stage {
    s.stage(&ItemId::new(id).unwrap()).unwrap()
}

#[test]
fn an_old_log_folds_to_the_states_it_always_did() {
    let log = log();
    let mid = agents_core::work_fold::fold(&log[..9]);
    assert_eq!(stage(&mid, "r1"), Stage::Held);
    assert_eq!(stage(&mid, "c1"), Stage::Ready);
    assert_eq!(stage(&mid, "c2"), Stage::Blocked);
    assert_eq!(stage(&mid, "r2"), Stage::Blocked);

    let end = agents_core::work_fold::fold(&log);
    // The agent's link of c1 to r2 is refused: r2 is another tree.
    assert_eq!(end.rejections.len(), 1);
    assert_eq!(stage(&end, "r1"), Stage::Done);
    assert_eq!(stage(&end, "c1"), Stage::Done);
    assert_eq!(stage(&end, "c2"), Stage::Closed);
    assert_eq!(stage(&end, "r2"), Stage::Ready);
    // No edge in an old log waits for a release.
    assert!(end.release_edges.is_empty());
}
