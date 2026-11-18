//! Delegated workers: a supervisor registers worker names that act for it
//! (format 21).

use super::*;
use crate::work::FORMAT_V21;

const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn started(worker: &str, hash: &str) -> WorkRecord {
    WorkRecord::WorkerStarted {
        worker: key(worker),
        token_sha256: hash.into(),
        vendor: Some("fake".into()),
        profile: Some("fake".into()),
        model: Some("small".into()),
    }
}

fn stopped(worker: &str) -> WorkRecord {
    WorkRecord::WorkerStopped {
        worker: key(worker),
    }
}

/// `by` acting for `sup`, as the tracker writes a worker's record.
fn as_worker(l: &mut Log, by: &str, sup: &str, record: WorkRecord) -> u64 {
    let offset = l.0.len() as u64;
    let rec =
        WorkLogged::new(offset * 10, key(by), Actor::Agent, record).with_supervisor(Some(key(sup)));
    l.0.push((offset, rec));
    offset
}

fn raised() -> Log {
    let mut l = Log::default();
    l.person(WorkRecord::FormatRaised { to: FORMAT_V21 });
    l.tracker(imported("r1"));
    l.person(WorkRecord::Accepted {
        item: id("r1"),
        tier: None,
        triage_skipped: None,
    });
    l
}

#[test]
fn a_worker_record_needs_format_21_and_the_raise() {
    let mut l = Log::default();
    l.agent("sup", started("sup_w1", HASH_A));
    assert_eq!(last_rejection(&l), Some(Reject::Format));
    let rec = WorkLogged::new(0, key("sup_w1"), Actor::Agent, claimed("r1", 1))
        .with_supervisor(Some(key("sup")));
    assert_eq!(rec.v, FORMAT_V21);
    // A record that names a supervisor but claims an older format is refused.
    let mut l = raised();
    let mut old = rec.clone();
    old.v = 19;
    l.0.push((3, old));
    assert_eq!(last_rejection(&l), Some(Reject::Format));
}

#[test]
fn a_worker_claims_and_its_records_name_the_supervisor() {
    let mut l = raised();
    l.agent("sup", started("sup_w1", HASH_A));
    as_worker(&mut l, "sup_w1", "sup", claimed("r1", 1));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    assert_eq!(s.workers[&key("sup_w1")].supervisor, key("sup"));
    assert_eq!(s.worker_by_token(HASH_A).unwrap().0, &key("sup_w1"));
    assert_eq!(s.live_workers(&key("sup")).count(), 1);
}

#[test]
fn a_worker_cannot_write_without_its_supervisor_or_for_another() {
    let mut l = raised();
    l.agent("sup", started("sup_w1", HASH_A));
    l.agent("sup_w1", claimed("r1", 1));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    as_worker(&mut l, "sup_w1", "other", claimed("r1", 1));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    // A name nobody registered cannot act for anyone.
    as_worker(&mut l, "sup_w9", "sup", claimed("r1", 1));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
}

#[test]
fn workers_cannot_supervise_and_names_belong_to_their_supervisor() {
    let mut l = raised();
    l.agent("sup", started("sup_w1", HASH_A));
    as_worker(&mut l, "sup_w1", "sup", started("sup_w1_x", HASH_B));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    l.agent("sup", started("other_w1", HASH_B));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    l.agent("sup", started("sup_", HASH_B));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    l.agent("sup", started("sup_w2", "not-a-hash"));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    l.agent("sup", started("sup_w2", HASH_A));
    assert_eq!(last_rejection(&l), Some(Reject::Duplicate));
    l.person(started("sup_w2", HASH_B));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
}

#[test]
fn a_stopped_worker_is_done_for_good() {
    let mut l = raised();
    l.agent("sup", started("sup_w1", HASH_A));
    l.agent("other", stopped("sup_w1"));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    l.agent("sup", stopped("sup_w1"));
    let s = fold(&l.0);
    assert!(s.worker_by_token(HASH_A).is_none());
    assert_eq!(s.live_workers(&key("sup")).count(), 0);
    as_worker(&mut l, "sup_w1", "sup", claimed("r1", 1));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    // The name is never reused.
    l.agent("sup", started("sup_w1", HASH_B));
    assert_eq!(last_rejection(&l), Some(Reject::Duplicate));
    l.agent("sup", stopped("sup_w1"));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    // A person may stop any worker.
    l.agent("sup", started("sup_w2", HASH_B));
    l.person(stopped("sup_w2"));
    assert_eq!(fold(&l.0).live_workers(&key("sup")).count(), 0);
}

#[test]
fn a_supervisor_has_at_most_the_fold_cap_of_live_workers() {
    let mut l = raised();
    for n in 0..crate::work::MAX_WORKERS_PER_SUPERVISOR {
        l.agent("sup", started(&format!("sup_w{n}"), &format!("{n:064x}")));
    }
    assert!(fold(&l.0).rejections.is_empty());
    l.agent("sup", started("sup_wx", HASH_A));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    l.agent("sup", stopped("sup_w0"));
    l.agent("sup", started("sup_wx", HASH_A));
    assert_eq!(fold(&l.0).rejections.len(), 1);
}
