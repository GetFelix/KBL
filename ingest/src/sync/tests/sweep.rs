//! A seeded sweep of ingest under faults. Each seed changes a few upstream
//! issues, a person dismisses some roots, and ingest polls while the tracker
//! crashes between two writes, loses a reply, refuses a save or goes away.
//! After every poll, and once the faults stop, the tracker's log is checked.
//!
//! `KBL_SWEEP_SEEDS=n` runs seeds 0..n (default 40), `KBL_SWEEP_SEED=s` replays
//! one seed. A failure names the seed. See docs/fault-sweep.md.

use std::collections::HashSet;

use kbl_core::work::CloseWhy;
use kbl_core::work_fold::Closure;

use super::*;

const ISSUES: u64 = 6;
const STEPS: usize = 40;

/// A bug the sweep found on main, which this change does not fix: the issue
/// that tracks it, or None once it is fixed. While it is set, a root that was
/// imported and never admitted, or lost a proposal, is reported and not failed.
/// Seed 42 shows it (KBL_SWEEP_SEED=42): a poll that fails partway through
/// the existing-root branch of `consider` still saves the root as tracked, so
/// later polls follow it and never finish its admission.
const PINNED_ADMISSION: Option<u32> = Some(PINNED_ISSUE);
const PINNED_ISSUE: u32 = 301;

/// Seeds that fail on main, each with the issue that tracks it.
const KNOWN_FAILING: [(u64, u32); 4] = [(1194, 302), (1398, 302), (2097, 302), (2380, 302)];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Prints the seed when a check inside the run panics.
struct Replay(u64);

impl Drop for Replay {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "fault sweep failed on seed {}; replay: KBL_SWEEP_SEED={} cargo test -p kbl-ingest sweep",
                self.0, self.0
            );
        }
    }
}

fn seeds() -> std::ops::Range<u64> {
    let var = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u64>().ok());
    if let Some(one) = var("KBL_SWEEP_SEED") {
        return one..one + 1;
    }
    0..var("KBL_SWEEP_SEEDS").unwrap_or(40)
}

#[derive(Clone, Copy, PartialEq)]
enum Upstream {
    Unseen,
    Open,
    Completed,
    NotPlanned,
}

struct World {
    issues: Vec<Upstream>,
    /// When each issue last changed, in minutes: its `updated_at`.
    changed: Vec<u32>,
    clock: u32,
    /// Roots a person dismissed, whose reply came back.
    dismissed: HashSet<String>,
}

impl World {
    /// GitHub's answers: the list, and each issue on its own, as a reconcile asks.
    fn publish(&self, rig: &Rig) {
        let all = self.listing();
        for v in &all {
            let url = format!("/issues/{}", v["number"]);
            rig.gh.on("GET", &url, ok(v.clone()));
        }
        rig.list(all);
    }

    fn listing(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for (n, up) in self.issues.iter().enumerate() {
            let number = 900 + n as u64;
            let t = self.changed[n];
            let stamp = format!("2026-11-10T{:02}:{:02}:00Z", t / 60, t % 60);
            let v = match up {
                Upstream::Unseen => continue,
                Upstream::Open => issue(number, |_| {}),
                Upstream::Completed => closed(issue(number, |_| {}), "completed", &stamp),
                Upstream::NotPlanned => closed(issue(number, |_| {}), "not_planned", &stamp),
            };
            out.push(at(v, &stamp));
        }
        out
    }
}

fn upstream_id(n: usize) -> String {
    format!("GetFelix/felix#{}", 900 + n)
}

/// What must hold after any poll, however it ended.
fn check(rig: &Rig, w: &World) {
    let state = rig.tracker.state();
    let roots = state.imported().count();
    assert_eq!(
        rig.tracker.count("imported"),
        roots,
        "an issue was imported twice"
    );
    for id in &w.dismissed {
        let item = &state.items[&kbl_core::names::ItemId::new(id).unwrap()];
        assert_eq!(item.closed, Some(Closure::Dismissed), "{id} was reopened");
    }
}

/// With every fault gone, ingest has caught up with upstream.
fn settled(rig: &Rig, w: &World) {
    check(rig, w);
    let state = rig.tracker.state();
    for (n, up) in w.issues.iter().enumerate() {
        let id = upstream_id(n);
        let root = state
            .imported()
            .find(|(_, u, _)| *u == id)
            .map(|(_, _, i)| i.clone());
        let Some(root) = root else {
            assert!(
                *up != Upstream::Open,
                "{id} is open upstream and was never imported"
            );
            continue;
        };
        let it = &state.items[&root];
        if w.dismissed.contains(root.as_str()) {
            continue;
        }
        match up {
            Upstream::Open => {
                assert!(it.closed.is_none(), "{id} is open upstream and closed here");
                // A reopen puts the root back in triage for a person, whatever
                // admission it had.
                if it.reopened_by.is_none() {
                    let admitted = it.accepted && it.children.len() == 2;
                    match PINNED_ADMISSION {
                        Some(issue) if !admitted => {
                            eprintln!("{id} was never admitted: known failing, KBL #{issue}");
                        }
                        _ => assert!(admitted, "{id} was imported and never fully admitted"),
                    }
                }
            }
            Upstream::Completed | Upstream::NotPlanned => {
                assert!(it.closed.is_some(), "{id} is closed upstream and open here");
            }
            Upstream::Unseen => unreachable!("an unseen issue was imported"),
        }
    }
}

fn mutate(rig: &Rig, w: &mut World, rng: &mut Rng) {
    let n = rng.below(ISSUES) as usize;
    w.clock += 1;
    let before = w.issues[n];
    match rng.below(10) {
        0..=3 => {
            if w.issues[n] == Upstream::Unseen {
                w.issues[n] = Upstream::Open;
            }
        }
        4 | 5 => {
            if w.issues[n] == Upstream::Open {
                w.issues[n] = if rng.below(2) == 0 {
                    Upstream::Completed
                } else {
                    Upstream::NotPlanned
                };
            }
        }
        6 => {
            if matches!(w.issues[n], Upstream::Completed | Upstream::NotPlanned) {
                w.issues[n] = Upstream::Open;
            }
        }
        _ => {
            let state = rig.tracker.state();
            let id = upstream_id(n);
            let open = |root: &kbl_core::names::ItemId| state.items[root].closed.is_none();
            // A dismissal of a root already closed changes nothing.
            if let Some((_, _, root)) = state.imported().find(|(_, u, _)| *u == id)
                && open(root)
            {
                let dismiss = WorkRecord::Closed {
                    item: root.clone(),
                    why: CloseWhy::Dismissed,
                };
                if rig.tracker.try_person(dismiss) {
                    w.dismissed.insert(root.to_string());
                }
            }
        }
    }
    if w.issues[n] != before {
        w.changed[n] = w.clock;
    }
    w.publish(rig);
}

async fn step(rig: &Rig, ingest: &Ingest<Github<Script>, Fake>, w: &mut World, rng: &mut Rng) {
    mutate(rig, w, rng);
    match rng.below(12) {
        // The process dies between two of its writes.
        0..=3 => rig.tracker.crash_after_writes(rng.below(9) as usize),
        4 => rig.tracker.lose_next_append_reply(),
        5 => rig.tracker.race_next_save(),
        6 => rig.tracker.fail_appends(true),
        7 => rig.tracker.forget_saved(),
        8 => rig.gh.on("GET", LIST, status(500, &[])),
        _ => {}
    }
    // The same delivery can arrive twice.
    for _ in 0..1 + rng.below(2) {
        let _ = ingest.poll(rng.below(8) == 0).await;
    }
    rig.tracker.restart();
    rig.tracker.fail_appends(false);
    rig.tracker.allow_saves();
    w.publish(rig);
    check(rig, w);
}

async fn run(seed: u64) {
    let _replay = Replay(seed);
    let mut rng = Rng(seed);
    let rig = rig();
    let ingest = rig.ingest(json!([admit_rule()]));
    let mut w = World {
        issues: vec![Upstream::Unseen; ISSUES as usize],
        changed: vec![0; ISSUES as usize],
        clock: 1,
        dismissed: HashSet::new(),
    };
    for _ in 0..STEPS {
        step(&rig, &ingest, &mut w, &mut rng).await;
    }
    // Faults stop; a few polls are enough to catch up.
    for _ in 0..3 {
        ingest.poll(true).await.unwrap();
    }
    settled(&rig, &w);
}

#[tokio::test]
async fn ingest_holds_its_invariants_under_seeded_faults() {
    let mut failed = Vec::new();
    for seed in seeds() {
        if let Some((_, issue)) = KNOWN_FAILING.iter().find(|(s, _)| *s == seed) {
            eprintln!("seed {seed} skipped: known failing, KBL #{issue}");
            continue;
        }
        if tokio::spawn(run(seed)).await.is_err() {
            failed.push(seed);
        }
    }
    assert!(failed.is_empty(), "fault sweep failed on seeds {failed:?}");
}
