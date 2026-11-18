//! Delegated workers: a supervisor registers worker names that act for it
//! (format 21).

use super::*;
use crate::work::FORMAT_V21;

const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const HASH_SUP: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn started(worker: &str, hash: &str) -> WorkRecord {
    WorkRecord::WorkerStarted {
        worker: key(worker),
        token_sha256: hash.into(),
        supervisor_token_sha256: HASH_SUP.into(),
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
    l.agent("sup", started("sup-w1", HASH_A));
    assert_eq!(last_rejection(&l), Some(Reject::Format));
    let rec = WorkLogged::new(0, key("sup-w1"), Actor::Agent, claimed("r1", 1))
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
    l.agent("sup", started("sup-w1", HASH_A));
    as_worker(&mut l, "sup-w1", "sup", claimed("r1", 1));
    let s = fold(&l.0);
    assert!(s.rejections.is_empty(), "{:?}", s.rejections);
    assert_eq!(s.workers[&key("sup-w1")].supervisor, key("sup"));
    assert_eq!(s.worker_by_token(HASH_A).unwrap().0, &key("sup-w1"));
    assert_eq!(s.live_workers(&key("sup")).count(), 1);
}

#[test]
fn a_worker_cannot_write_without_its_supervisor_or_for_another() {
    let mut l = raised();
    l.agent("sup", started("sup-w1", HASH_A));
    l.agent("sup-w1", claimed("r1", 1));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    as_worker(&mut l, "sup-w1", "other", claimed("r1", 1));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    // A name nobody registered cannot act for anyone.
    as_worker(&mut l, "sup-w9", "sup", claimed("r1", 1));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
}

#[test]
fn workers_cannot_supervise_and_names_belong_to_their_supervisor() {
    let mut l = raised();
    l.agent("sup", started("sup-w1", HASH_A));
    as_worker(&mut l, "sup-w1", "sup", started("sup-w1-x", HASH_B));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    l.agent("sup", started("other-w1", HASH_B));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    l.agent("sup", started("sup-", HASH_B));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    l.agent("sup", started("sup-w2", "not-a-hash"));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    l.agent("sup", started("sup-w2", HASH_A));
    assert_eq!(last_rejection(&l), Some(Reject::Duplicate));
    l.person(started("sup-w2", HASH_B));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
}

#[test]
fn a_stopped_worker_is_done_for_good() {
    let mut l = raised();
    l.agent("sup", started("sup-w1", HASH_A));
    l.agent("other", stopped("sup-w1"));
    assert_eq!(last_rejection(&l), Some(Reject::ActorNotAllowed));
    l.agent("sup", stopped("sup-w1"));
    let s = fold(&l.0);
    assert!(s.worker_by_token(HASH_A).is_none());
    assert_eq!(s.live_workers(&key("sup")).count(), 0);
    as_worker(&mut l, "sup-w1", "sup", claimed("r1", 1));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    // The name is never reused.
    l.agent("sup", started("sup-w1", HASH_B));
    assert_eq!(last_rejection(&l), Some(Reject::Duplicate));
    l.agent("sup", stopped("sup-w1"));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    // A person may stop any worker.
    l.agent("sup", started("sup-w2", HASH_B));
    l.person(stopped("sup-w2"));
    assert_eq!(fold(&l.0).live_workers(&key("sup")).count(), 0);
}

#[test]
fn a_supervisor_has_at_most_the_fold_cap_of_live_workers() {
    let mut l = raised();
    for n in 0..crate::work::MAX_WORKERS_PER_SUPERVISOR {
        l.agent("sup", started(&format!("sup-w{n}"), &format!("{n:064x}")));
    }
    assert!(fold(&l.0).rejections.is_empty());
    l.agent("sup", started("sup-wx", HASH_A));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    l.agent("sup", stopped("sup-w0"));
    l.agent("sup", started("sup-wx", HASH_A));
    assert_eq!(fold(&l.0).rejections.len(), 1);
}

fn note(item: &str) -> WorkRecord {
    WorkRecord::Progress {
        item: id(item),
        attempt: 1,
        note: text("on it"),
        branch: None,
        pr: None,
    }
}

#[test]
fn a_worker_name_is_split_at_its_first_dash_and_belongs_to_one_supervisor() {
    let mut l = raised();
    // `sup_x-w1` is a worker of `sup_x`, never of `sup`.
    l.agent("sup", started("sup_x-w1", HASH_A));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    l.agent("sup", started("sup-w1-x", HASH_A));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    l.agent("sup", started("sup_w1", HASH_A));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    let mut bad_sup = started("sup-w1", HASH_A);
    if let WorkRecord::WorkerStarted {
        supervisor_token_sha256,
        ..
    } = &mut bad_sup
    {
        *supervisor_token_sha256 = "nope".into();
    }
    l.agent("sup", bad_sup);
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    l.agent("sup", started("sup-w1", HASH_A));
    let s = fold(&l.0);
    assert_eq!(s.rejections.len(), 4, "{:?}", s.rejections);
    assert_eq!(s.workers[&key("sup-w1")].supervisor_token_sha256, HASH_SUP);
}

#[test]
fn no_agent_writes_under_a_worker_name_before_or_after_the_worker() {
    // Agent first: an agent record under a worker-shaped name is refused.
    let mut l = raised();
    l.agent("sup-w1", claimed("r1", 1));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    // Worker first: the name is the worker's, and only it writes under it.
    l.agent("sup", started("sup-w1", HASH_A));
    l.agent("sup-w1", claimed("r1", 1));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    // Stopped, then the name comes back as an agent or a worker: refused both ways.
    l.agent("sup", stopped("sup-w1"));
    l.agent("sup-w1", claimed("r1", 1));
    assert_eq!(last_rejection(&l), Some(Reject::BadWorker));
    l.agent("sup", started("sup-w1", HASH_B));
    assert_eq!(last_rejection(&l), Some(Reject::Duplicate));
    assert_eq!(fold(&l.0).rejections.len(), 4);
}

#[test]
fn a_workers_hold_is_its_own() {
    let mut l = raised();
    l.agent("sup", started("sup-w1", HASH_A));
    l.agent("sup", started("sup-w2", HASH_B));
    as_worker(&mut l, "sup-w1", "sup", claimed("r1", 1));
    let clean = fold(&l.0).rejections.len();
    // Neither the supervisor nor a sibling worker acts on the worker's hold.
    l.agent("sup", note("r1"));
    as_worker(&mut l, "sup-w2", "sup", note("r1"));
    let s = fold(&l.0);
    assert_eq!(s.rejections.len(), clean + 2, "{:?}", s.rejections);
    assert_eq!(s.items[&id("r1")].hold.as_ref().unwrap().by, key("sup-w1"));
}

fn release(reason: &str) -> WorkRecord {
    WorkRecord::Released {
        item: id("r1"),
        attempt: 1,
        reason: Some(reason.into()),
        why: None,
        cost_micros: 0,
        usage: vec![],
        floor_micros: 0,
    }
}

#[test]
fn a_hold_released_because_the_supervisor_went_away_costs_no_try() {
    let tries = |reason: &str| {
        let mut l = raised();
        l.agent("sup", started("sup-w1", HASH_A));
        as_worker(&mut l, "sup-w1", "sup", claimed("r1", 1));
        l.tracker(stopped("sup-w1"));
        l.tracker(release(reason));
        let s = fold(&l.0);
        assert!(s.rejections.is_empty(), "{:?}", s.rejections);
        let it = &s.items[&id("r1")];
        assert!(it.hold.is_none());
        assert!(s.worker_by_token(HASH_A).is_none());
        it.paths[0].tries
    };
    assert_eq!(tries(crate::work::SUPERVISOR_GONE), 0);
    assert_eq!(tries("revoked"), 1);
}

#[test]
fn format_20_is_not_read_by_this_build() {
    let mut l = Log::default();
    let mut raise = WorkLogged::new(
        0,
        key("gabriel"),
        Actor::Person,
        WorkRecord::FormatRaised { to: 20 },
    );
    raise.v = 20;
    l.0.push((0, raise));
    assert_eq!(last_rejection(&l), Some(Reject::Format));
    let mut l = raised();
    let mut rec = WorkLogged::new(30, key("w1"), Actor::Agent, claimed("r1", 1));
    rec.v = 20;
    l.0.push((3, rec));
    assert_eq!(last_rejection(&l), Some(Reject::Format));
}
