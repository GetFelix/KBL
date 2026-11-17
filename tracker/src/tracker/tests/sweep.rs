//! A seeded sweep of the claim path under faults. Each seed drives imports,
//! claims, completions, dismissals and restarts against a Raft-style `Fake`
//! while the broker misbehaves, and checks the log after every step.
//!
//! `KBL_SWEEP_SEEDS=n` runs seeds 0..n (default 40), `KBL_SWEEP_SEED=s` replays
//! one seed. A failure names the seed. See docs/fault-sweep.md.

use kbl_core::work::CloseWhy;
use kbl_core::work_fold::{Closure, Stage, State};

use super::*;

const AGENTS: [&str; 3] = ["w1", "w2", "w3"];
const STEPS: usize = 70;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Prints the seed when a check inside the run panics.
struct Replay(u64);

impl Drop for Replay {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "fault sweep failed on seed {}; replay: KBL_SWEEP_SEED={} cargo test -p kbl-tracker sweep",
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

/// The stream as the log's readers see it, folded from nothing. Panics on a
/// record the fold refuses, and on a claim of an item that is already held.
fn replay(fake: &Fake) -> State {
    let records = fake
        .net
        .lock()
        .unwrap()
        .streams
        .get(WORK_EVENTS)
        .cloned()
        .unwrap_or_default();
    let mut state = State::default();
    let mut dones: HashSet<(String, u32)> = HashSet::new();
    for (offset, payload) in records {
        let logged = WorkLogged::decode(&payload).unwrap();
        match &logged.record {
            WorkRecord::Claimed { item, .. } => {
                let held = state.items.get(item).and_then(|i| i.hold.as_ref());
                assert!(held.is_none(), "{item} claimed at {offset} while held");
            }
            WorkRecord::Done { item, attempt, .. } => {
                assert!(
                    dones.insert((item.to_string(), *attempt)),
                    "{item} attempt {attempt} done twice"
                );
            }
            _ => {}
        }
        state
            .try_apply(offset, &logged)
            .unwrap_or_else(|why| panic!("the fold refused the record at {offset}: {why:?}"));
    }
    state
}

#[derive(Default)]
struct Model {
    next_item: u32,
    /// What each agent believes it holds: item and attempt.
    beliefs: HashMap<&'static str, String>,
    /// Who was handed each claim, by the offset of its `claimed` record.
    handed: HashMap<u64, &'static str>,
    /// Items whose import was acknowledged, and whose accept was.
    imported: HashSet<String>,
    accepted: HashSet<String>,
    dismissed: HashSet<String>,
    done: HashSet<String>,
    /// Records another broker's writer committed.
    foreign: u32,
}

impl Model {
    fn check(&self, fake: &Fake) {
        let state = replay(fake);
        for id in &self.dismissed {
            let it = &state.items[&item(id)];
            assert_eq!(it.closed, Some(Closure::Dismissed), "{id} was reopened");
            assert!(it.hold.is_none(), "{id} is held after its dismissal");
        }
        for id in &self.imported {
            assert!(state.items.contains_key(&item(id)), "{id} was lost");
        }
    }
}

async fn restart(fake: &Fake) -> Tracker<Fake> {
    start(fake).await
}

fn ready_streams() -> [String; 4] {
    [0u8, 1, 2, 3].map(ready_stream)
}

async fn seed_item(t: &Tracker<Fake>, m: &mut Model) {
    let id = format!("i{}", m.next_item);
    m.next_item += 1;
    let put = |record| t.person_record(key("gab"), record);
    if put(imported(&id)).await.is_err() {
        return;
    }
    m.imported.insert(id.clone());
    let accepted = WorkRecord::Accepted {
        item: item(&id),
        tier: Some(2),
    };
    if put(accepted).await.is_ok() {
        m.accepted.insert(id);
    }
}

async fn claim_step(t: &Tracker<Fake>, fake: &Fake, m: &mut Model, who: &'static str) {
    let Ok(reply) = t.next(&agent(who), Duration::ZERO).await else {
        return;
    };
    let Some(id) = claimed_id(&reply) else {
        return;
    };
    let attempt = reply["attempt"].as_u64().unwrap() as u32;
    let at = reply["offset"].as_u64().expect("a claim names its record");
    let resumed = reply["resumed"] == true;
    let state = replay(fake);
    let hold = state.items[&item(&id)]
        .hold
        .clone()
        .expect("a claim with no hold");
    assert_eq!(
        (hold.by.as_str(), hold.attempt),
        (who, attempt),
        "{who} was told it holds {id} attempt {attempt}"
    );
    // A resume may be of a claim whose first reply was lost; a fresh claim is
    // only ever handed out once.
    match m.handed.insert(at, who) {
        Some(other) if resumed => assert_eq!(other, who, "{who} resumed {other}'s claim on {id}"),
        Some(other) => panic!("{id} claim at {at} went to {other} and to {who}"),
        None => {}
    }
    m.beliefs.insert(who, id);
}

async fn finish_step(t: &Tracker<Fake>, m: &mut Model, rng: &mut Rng, who: &'static str) {
    let Some(id) = m.beliefs.get(who).cloned() else {
        return;
    };
    let a = agent(who);
    let sent = match rng.below(3) {
        0 => {
            let r = t.complete(&a, &item(&id), None, Vec::new()).await;
            r.map(|_| true)
        }
        1 => t
            .release(&a, &item(&id), None, None, Vec::new())
            .await
            .map(|_| false),
        _ => t
            .give_up(&a, &item(&id), None, "stuck".into(), Vec::new())
            .await
            .map(|_| false),
    };
    if let Ok(done) = sent {
        m.beliefs.remove(who);
        if done {
            m.done.insert(id);
        }
    }
}

async fn dismiss_step(t: &Tracker<Fake>, m: &mut Model, rng: &mut Rng) {
    let mut ids: Vec<_> = m.imported.iter().cloned().collect();
    ids.sort();
    if ids.is_empty() {
        return;
    }
    let id = ids[rng.below(ids.len())].clone();
    let closed = WorkRecord::Closed {
        item: item(&id),
        why: CloseWhy::Dismissed,
    };
    if t.person_record(key("gab"), closed).await.is_ok() {
        m.dismissed.insert(id);
    }
}

/// One step: maybe a fault, then an operation, then the checks.
async fn step(fake: &Fake, t: &mut Tracker<Fake>, m: &mut Model, rng: &mut Rng) {
    let mut crash_after = false;
    match rng.below(100) {
        0..=7 => {
            // The tracker dies after some number of its writes.
            fake.fail_publishes_to_after(WORK_EVENTS, rng.below(3) as u32);
            crash_after = true;
        }
        8..=12 => *t = restart(fake).await,
        13..=17 => fake.lose_next_publish_reply(WORK_EVENTS),
        18..=22 => {
            // Another broker's writer lands first: a failover. The tracker's
            // conditional publish is refused and, on a Raft shard, burns an offset.
            m.foreign += 1;
            let theirs = WorkLogged::new(
                0,
                key("gab"),
                Actor::Person,
                imported(&format!("f{}", m.foreign)),
            );
            fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
        }
        23..=30 => {
            // Felix hands a record out again: a lapsed lease, or a repeat of an acked one.
            let streams = ready_streams();
            let stream = &streams[rng.below(4)];
            let len = fake.ready_count(stream);
            if len > 0 {
                fake.lapse(stream, rng.below(len) as u64);
            }
        }
        31..=34 => fake.fail_polls(Some("work.ready")),
        35..=40 => {
            pass(t, Duration::from_secs(10 + rng.below(90) as u64)).await;
            t.sweep().await;
        }
        _ => {}
    }
    let who = AGENTS[rng.below(AGENTS.len())];
    match rng.below(10) {
        0..=1 => seed_item(t, m).await,
        2..=5 => claim_step(t, fake, m, who).await,
        6..=8 => finish_step(t, m, rng, who).await,
        _ => dismiss_step(t, m, rng).await,
    }
    fake.fail_polls(None);
    if crash_after {
        fake.lift_publish_budget();
        *t = restart(fake).await;
    }
    m.check(fake);
}

/// Faults stop, a tracker restarts, and the agents drain the queue.
async fn heal(fake: &Fake, m: &mut Model) {
    fake.lift_publish_budget();
    fake.fail_polls(None);
    let t = restart(fake).await;
    // A person retries an accept that was not acknowledged.
    for id in m.imported.clone() {
        if m.accepted.contains(&id) || m.dismissed.contains(&id) {
            continue;
        }
        let accepted = WorkRecord::Accepted {
            item: item(&id),
            tier: Some(2),
        };
        let _ = t.person_record(key("gab"), accepted).await;
        m.accepted.insert(id);
    }
    // A poll may only drop a stale record, so a few empty rounds in a row end it.
    let mut quiet = 0;
    for _ in 0..300 {
        pass(&t, VIS + Duration::from_secs(5)).await;
        // Felix's visibility timeout passes for every delivery nobody settled.
        fake.lapse_all();
        t.sweep().await;
        let mut idle = true;
        for who in AGENTS {
            claim_step(&t, fake, m, who).await;
            if let Some(id) = m.beliefs.get(who).cloned() {
                idle = false;
                if t.complete(&agent(who), &item(&id), None, Vec::new())
                    .await
                    .is_ok()
                {
                    m.beliefs.remove(who);
                    m.done.insert(id);
                }
            }
        }
        m.check(fake);
        quiet = if idle { quiet + 1 } else { 0 };
        if quiet == 6 {
            break;
        }
    }
    let state = replay(fake);
    let log = fake.log();
    for id in &m.accepted {
        if m.dismissed.contains(id) {
            continue;
        }
        let finished = log
            .iter()
            .any(|l| matches!(&l.record, WorkRecord::Done { item: i, .. } if i == &item(id)));
        // A person may have dismissed an item whose reply was lost.
        let closed = state.items[&item(id)].closed.is_some();
        // Out of claims or given up on twice, an item waits for a person.
        let waits = state.stage(&item(id)) == Some(Stage::NeedsYou);
        assert!(
            finished || closed || waits,
            "{id} was accepted and never worked (stage {:?})",
            state.stage(&item(id))
        );
    }
}

#[tokio::test(start_paused = true)]
async fn the_claim_path_holds_its_invariants_under_seeded_faults() {
    for seed in seeds() {
        let _replay = Replay(seed);
        let mut rng = Rng(seed);
        let fake = Fake::default();
        // Half the seeds run on a Raft shard, where a refusal burns an offset.
        if seed % 2 == 0 {
            fake.raft_blanks();
        }
        let mut t = start(&fake).await;
        let mut m = Model::default();
        for _ in 0..STEPS {
            step(&fake, &mut t, &mut m, &mut rng).await;
        }
        heal(&fake, &mut m).await;
    }
}
