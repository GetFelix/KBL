//! A seeded sweep of the claim path under faults. Each seed drives imports,
//! claims, extensions, completions, dismissals and restarts against a Raft-style `Fake`
//! while the broker misbehaves, and checks the log after every step.
//!
//! `KBL_SWEEP_SEEDS=n` runs seeds 0..n (default 40), `KBL_SWEEP_SEED=s` replays
//! one seed. A failure names the seed. See docs/fault-sweep.md.

use kbl_core::content::Content;
use kbl_core::org::TokenHash;
use kbl_core::work::{Assignee, CloseWhy, ReleaseWhy};
use kbl_core::work_fold::{Closure, Stage, State};

use super::*;
use crate::tracker::NewWorker;

const AGENTS: [&str; 3] = ["w1", "w2", "w3"];
/// Names the supervisor `sup` starts workers under: more than its cap allows
/// at once, so a stop frees a slot for the next.
const WORKERS: [&str; 3] = ["sup-a", "sup-b", "sup-c"];
/// Everyone who claims: agents, and workers once they are started.
const CLAIMANTS: [&str; 6] = ["w1", "w2", "w3", "sup-a", "sup-b", "sup-c"];
const STEPS: usize = 70;
/// Claims of work that needs something, across the seeds, so the sweep
/// cannot stop testing the rule without saying so.
static NEEDY_CLAIMS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The needs an item can carry and the capabilities an agent can declare.
const NEEDS: [&str; 3] = ["azure", "gpu", "network"];

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

/// The log folded from nothing, and the offsets of the claims whose hold has
/// since ended.
struct Replayed {
    state: State,
    ended: HashSet<u64>,
}

/// The stream as the log's readers see it, folded from nothing. Panics on a
/// record the fold refuses, and on a claim of an item that is already held.
fn replay(fake: &Fake) -> Replayed {
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
    let mut open: HashMap<ItemId, u64> = HashMap::new();
    let mut ended = HashSet::new();
    for (offset, payload) in records {
        let logged = WorkLogged::decode(&payload).unwrap();
        match &logged.record {
            WorkRecord::Claimed { item, .. } => {
                let held = state.items.get(item).and_then(|i| i.hold.as_ref());
                assert!(held.is_none(), "{item} claimed at {offset} while held");
                assert!(
                    !state.blocked_at(item, logged.at_ms),
                    "{item} claimed at {offset} while blocked"
                );
                if let Some(Assignee::Agent(a)) =
                    state.items.get(item).and_then(|i| i.assignee.as_ref())
                {
                    assert_eq!(
                        a, &logged.by,
                        "{item} claimed at {offset} by a non-assignee"
                    );
                }
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
        if let Some(id) = logged.record.item() {
            if matches!(logged.record, WorkRecord::Claimed { .. }) {
                open.insert(id.clone(), offset);
            } else if state.items.get(id).is_some_and(|i| i.hold.is_none()) {
                ended.extend(open.remove(id));
            }
        }
    }
    Replayed { state, ended }
}

/// The tracker agents talk to, and the one it replaced, which has not noticed
/// yet. Until its lease check or a refused write fences it, the old tracker
/// still serves from a state another writer has moved past.
struct Trackers {
    cur: Tracker<Fake>,
    old: Option<Tracker<Fake>>,
}

impl Trackers {
    fn serving(&self, old: bool) -> &Tracker<Fake> {
        match &self.old {
            Some(t) if old => t,
            _ => &self.cur,
        }
    }
}

#[derive(Default)]
struct Model {
    next_item: u32,
    /// What each agent believes it holds: item and attempt.
    beliefs: HashMap<&'static str, String>,
    /// The attempt of the claim each agent believes it holds.
    attempts: HashMap<&'static str, u32>,
    /// Who was handed each claim, by the offset of its `claimed` record.
    handed: HashMap<u64, &'static str>,
    /// Items whose import was acknowledged, and whose accept was.
    imported: HashSet<String>,
    accepted: HashSet<String>,
    dismissed: HashSet<String>,
    done: HashSet<String>,
    /// Records another broker's writer committed.
    foreign: u32,
    /// Who was handed each attempt of each item, and at which claim. Kept apart
    /// from the log so a claim that lands twice cannot hide behind the fold.
    started: HashMap<(String, u32), (String, u64)>,
    /// The token each worker was handed when its start was acknowledged.
    tokens: HashMap<&'static str, String>,
    /// Seed of the generator the needs and capability steps draw from.
    aux: u64,
    /// What each claimant declares on its next claim. It changes between
    /// claims, since capabilities ride each call.
    caps: HashMap<&'static str, Vec<String>>,
    /// The supervisor was removed: its workers are stopped and it starts no more.
    gone: bool,
}

impl Model {
    /// An attempt goes to a second agent only after the first one's hold ended.
    fn start(&mut self, ended: &HashSet<u64>, id: &str, attempt: u32, who: &str, at: u64) {
        let prior = self
            .started
            .insert((id.to_owned(), attempt), (who.to_owned(), at));
        if let Some((other, was)) = prior
            && other != who
        {
            assert!(
                was != at && ended.contains(&was),
                "{id} attempt {attempt} went to {other} (claim at {was}) and to {who} (claim at {at})"
            );
        }
    }

    fn check(&self, fake: &Fake) {
        // The fault model, not the tracker: KBL requires Felix to keep every
        // record it acknowledged (docs/work-tracker.md), so no fault cuts one.
        let cut = fake.cut_acked(WORK_EVENTS);
        assert_eq!(cut, None, "the acknowledged record at {cut:?} was cut");
        let state = replay(fake).state;
        for id in &self.dismissed {
            let it = &state.items[&item(id)];
            assert_eq!(it.closed, Some(Closure::Dismissed), "{id} was reopened");
            assert!(it.hold.is_none(), "{id} is held after its dismissal");
        }
        for id in &self.imported {
            assert!(state.items.contains_key(&item(id)), "{id} was lost");
        }
        // Every worker belongs to `sup` and its name says so, and the pool
        // stays inside the tracker's caps whatever the faults did.
        let cap = settings();
        for (name, w) in &state.workers {
            assert_eq!(
                w.supervisor.as_str(),
                "sup",
                "{name} has another supervisor"
            );
            assert!(name.as_str().starts_with("sup-"), "{name} is not sup's");
        }
        assert!(state.live_workers(&key("sup")).count() <= cap.max_workers);
        // A question frees its asker, so nobody holds an item it waits on.
        for it in state.items.values() {
            assert!(
                it.open_question.is_none() || it.hold.is_none(),
                "{} is held with an open question",
                it.id
            );
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
    // In the repo the needs vocabulary is set for.
    let record = WorkRecord::Imported {
        item: item(&id),
        source: "github".into(),
        upstream_id: format!("GetFelix/felix#{id}"),
        title: "t".into(),
        labels: vec![],
        milestone: None,
    };
    if put(record).await.is_err() {
        return;
    }
    m.imported.insert(id.clone());
    let accepted = WorkRecord::Accepted {
        item: item(&id),
        tier: Some(2),
        triage_skipped: None,
    };
    if put(accepted).await.is_ok() {
        m.accepted.insert(id);
    }
}

async fn claim_step(t: &Tracker<Fake>, fake: &Fake, m: &mut Model, who: &'static str) {
    claim_with(t, fake, m, who, &Filter::default()).await;
}

/// A claim that names kinds or tiers: it may find nothing, and what it finds is
/// still a claim the log accepts.
async fn filtered_claim_step(
    t: &Tracker<Fake>,
    fake: &Fake,
    m: &mut Model,
    rng: &mut Rng,
    who: &'static str,
) {
    let filter = match rng.below(3) {
        0 => Filter::parse(&["fix".into()], &[], None, &[]),
        1 => Filter::parse(&[], &[0, 1], None, &[]),
        _ => Filter::parse(&["triage".into()], &[], None, &[]),
    }
    .unwrap();
    claim_with(t, fake, m, who, &filter).await;
}

async fn claim_with(
    t: &Tracker<Fake>,
    fake: &Fake,
    m: &mut Model,
    who: &'static str,
    filter: &Filter,
) {
    // Until a claimer narrows what it declares, it declares everything.
    let caps = m
        .caps
        .get(who)
        .cloned()
        .unwrap_or_else(|| NEEDS.map(str::to_owned).to_vec());
    let filter = filter.clone().with_capabilities(caps.clone()).unwrap();
    let Ok(reply) = t
        .next_filtered(&agent(who), Duration::ZERO, &[], &filter)
        .await
    else {
        return;
    };
    let Some(id) = claimed_id(&reply) else {
        return;
    };
    let attempt = reply["attempt"].as_u64().unwrap() as u32;
    let at = reply["offset"].as_u64().expect("a claim names its record");
    let resumed = reply["resumed"] == true;
    let Replayed { state, ended } = replay(fake);
    assert!(
        !(resumed && ended.contains(&at)),
        "{who} was resumed {id}'s claim at {at}, whose hold had ended"
    );
    // Work goes only to a claimer whose declared capabilities cover what the
    // root needs. Read off the root, not through the fold's own check. A
    // resumed claim predates the call, so it is exempt.
    if !resumed {
        let it = &state.items[&item(&id)];
        if matches!(
            it.work,
            WorkKind::Fix | WorkKind::Test | WorkKind::Resume | WorkKind::FixCheck
        ) {
            let needs = state.items[&it.root].class.needs();
            if !needs.is_empty() {
                NEEDY_CLAIMS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            for need in needs {
                assert!(
                    caps.iter().any(|c| c.eq_ignore_ascii_case(need)),
                    "{who} declared {caps:?} and was handed {id}, which needs {need}"
                );
            }
        }
    }
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
    m.start(&ended, &id, attempt, who, at);
    m.beliefs.insert(who, id);
    m.attempts.insert(who, attempt);
}

async fn finish_step(t: &Tracker<Fake>, m: &mut Model, rng: &mut Rng, who: &'static str) {
    let Some(id) = m.beliefs.get(who).cloned() else {
        return;
    };
    let a = agent(who);
    let sent = match rng.below(4) {
        0 => {
            let r = t.complete(&a, &item(&id), None, Vec::new()).await;
            r.map(|_| true)
        }
        1 => t
            .release(&a, &item(&id), None, None, None, Vec::new())
            .await
            .map(|_| false),
        2 => t
            .release(
                &a,
                &item(&id),
                None,
                Some("no credit".into()),
                Some(ReleaseWhy::WaitsOnOutside),
                Vec::new(),
            )
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

/// An agent asks a person about what it believes it holds, which frees it.
async fn ask_step(t: &Tracker<Fake>, m: &mut Model, who: &'static str) {
    let Some(id) = m.beliefs.get(who).cloned() else {
        return;
    };
    let q = Content::Text("which region?".into());
    if t.ask(&agent(who), &item(&id), None, q, vec![], None, Vec::new())
        .await
        .is_ok()
    {
        m.beliefs.remove(who);
    }
}

/// A person answers an open question, which hands the item out again; a
/// person also moves a parked wait back to Ready.
async fn answer_step(t: &Tracker<Fake>, fake: &Fake, rng: &mut Rng) {
    let state = replay(fake).state;
    let open: Vec<_> = state
        .items
        .values()
        .filter(|i| i.open_question.is_some() || i.flagged)
        .map(|i| (i.id.clone(), i.open_question))
        .collect();
    if open.is_empty() {
        return;
    }
    let (id, question) = open[rng.below(open.len())].clone();
    let _ = match question {
        Some(q) => {
            let a = Content::Text("eastus".into());
            t.person_answer(key("gab"), &id, Some(q), a, vec![], Reply::default())
                .await
        }
        None => {
            let unflag = WorkRecord::Flagged {
                item: id,
                flagged: false,
            };
            t.person_record(key("gab"), unflag).await
        }
    };
}

/// An agent extends what it believes it holds. A tracker that missed the
/// end of that hold must not extend it.
async fn extend_step(t: &Tracker<Fake>, fake: &Fake, m: &mut Model, who: &'static str) {
    let Some(id) = m.beliefs.get(who).cloned() else {
        return;
    };
    let attempt = m.attempts[who];
    match t.extend(&agent(who), &item(&id)).await {
        Ok(_) => {
            let state = replay(fake).state;
            let hold = state.items[&item(&id)].hold.as_ref();
            let holder = hold.map(|h| (h.by.as_str(), h.attempt));
            assert_eq!(
                holder,
                Some((who, attempt)),
                "{who} was extended {id} attempt {attempt}, which the log shows held by {holder:?}"
            );
        }
        Err(Error::Lost(_)) => {
            m.beliefs.remove(who);
        }
        Err(_) => {}
    }
}

/// A person assigns an item to an agent, or to nobody, held or not.
async fn assign_step(t: &Tracker<Fake>, m: &mut Model, rng: &mut Rng) {
    let mut ids: Vec<_> = m.accepted.iter().cloned().collect();
    ids.sort();
    if ids.is_empty() {
        return;
    }
    let id = ids[rng.below(ids.len())].clone();
    let n = rng.below(AGENTS.len() + 1);
    let to = AGENTS.get(n).map(|a| Assignee::Agent(key(a)));
    let _ = t
        .person_record(
            key("gab"),
            WorkRecord::Reassigned {
                item: item(&id),
                to,
            },
        )
        .await;
}

/// A person blocks an accepted item on a resource, sometimes with a time that
/// stays far out, or lifts a block. Either may be refused; the check is that
/// no claim lands on an item while its block holds.
async fn block_step(t: &Tracker<Fake>, m: &mut Model, rng: &mut Rng, lift: bool) {
    let mut ids: Vec<_> = m.accepted.iter().cloned().collect();
    ids.sort();
    if ids.is_empty() {
        return;
    }
    let id = ids[rng.below(ids.len())].clone();
    let record = if lift {
        WorkRecord::BlockCleared { item: item(&id) }
    } else {
        WorkRecord::BlockSet {
            item: item(&id),
            until_ms: (rng.below(2) == 0).then(|| now_ms() + 30 * 86_400_000),
            resource: Some("azure-vms".into()),
            reason: "no credit".into(),
        }
    };
    let _ = t.person_record(key("gab"), record).await;
}

/// A person sets or clears what an accepted item needs, or an agent changes
/// what it declares. Either may be refused; the claim check judges the result.
async fn needs_step(t: &Tracker<Fake>, m: &mut Model, rng: &mut Rng) {
    let mut ids: Vec<_> = m.accepted.iter().cloned().collect();
    ids.sort();
    if ids.is_empty() {
        return;
    }
    let id = ids[rng.below(ids.len())].clone();
    // Sparse, so most work stays open to every claimer and workers still
    // hold items when they stop.
    let needs: Vec<String> = pick(rng, 4);
    let edit = WorkRecord::Edited {
        item: item(&id),
        title: None,
        tier: None,
        labels: None,
        milestone: None,
        item_type: None,
        area: None,
        size: None,
        needs: Some(needs),
        depends_on: None,
        split: None,
        proposal: None,
    };
    let _ = t.person_record(key("gab"), edit).await;
}

fn pick(rng: &mut Rng, one_in: usize) -> Vec<String> {
    NEEDS
        .iter()
        .filter(|_| rng.below(one_in) == 0)
        .map(|n| (*n).to_owned())
        .collect()
}

fn caps_step(m: &mut Model, rng: &mut Rng, who: &'static str) {
    m.caps.insert(who, pick(rng, 2));
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

/// Another broker's writer lands a record at the tail: a failover. Half the
/// time it is a claim or a release by another worker, which the fold accepts,
/// so a tracker that writes without its tail check lands a second claim.
async fn foreign_write(fake: &Fake, m: &mut Model, rng: &mut Rng) {
    m.foreign += 1;
    let Replayed { state, ended } = replay(fake);
    let at = fake.net.lock().unwrap().next_offset(WORK_EVENTS);
    let ids: Vec<ItemId> = state.items.keys().cloned().collect();
    let kind = rng.below(5);
    let first = rng.below(ids.len().max(1));
    let mut theirs = None;
    for k in 0..ids.len() {
        let id = &ids[(first + k) % ids.len()];
        let it = &state.items[id];
        if it.closed.is_some() {
            continue;
        }
        let (by, actor, record) = match (kind, &it.hold) {
            (0, None) => (
                key("ghost"),
                Actor::Agent,
                WorkRecord::Claimed {
                    item: id.clone(),
                    attempt: it.next_attempt(),
                    deadline_ms: now_ms() + 60_000,
                    direct: false,
                },
            ),
            // Another tracker's person assigns an agent this one has not folded.
            (2, _) => (
                key("gab"),
                Actor::Person,
                WorkRecord::Reassigned {
                    item: id.clone(),
                    to: Some(Assignee::Agent(key(AGENTS[rng.below(AGENTS.len())]))),
                },
            ),
            (1, Some(h)) => (
                h.by.clone(),
                Actor::Agent,
                WorkRecord::Released {
                    item: id.clone(),
                    attempt: h.attempt,
                    reason: None,
                    why: None,
                    cost_micros: 0,
                    usage: Vec::new(),
                    floor_micros: 0,
                },
            ),
            _ => continue,
        };
        let logged = WorkLogged::new(0, by, actor, record);
        if state.clone().try_apply(at, &logged).is_ok() {
            theirs = Some(logged);
            break;
        }
    }
    let theirs = theirs.unwrap_or_else(|| {
        WorkLogged::new(
            0,
            key("gab"),
            Actor::Person,
            imported(&format!("f{}", m.foreign)),
        )
    });
    let offset = fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
    if let WorkRecord::Claimed { item, attempt, .. } = &theirs.record {
        m.start(&ended, item.as_str(), *attempt, "ghost", offset);
    }
}

/// The holds that came from a direct claim: the claim says so, or the item was
/// assigned to its claimer when an older claim landed. By agent: item and attempt.
fn direct_holds(fake: &Fake) -> HashMap<String, (String, u32)> {
    let mut state = State::default();
    let mut direct = HashMap::new();
    for (offset, logged) in fake.log().into_iter().enumerate() {
        let before = state
            .items
            .get(logged.record.item().unwrap_or(&item("none")))
            .and_then(|i| i.assignee.clone());
        if state.try_apply(offset as u64, &logged).is_err() {
            continue;
        }
        if let WorkRecord::Claimed {
            item,
            attempt,
            direct: flagged,
            ..
        } = &logged.record
        {
            let mine = *flagged || before == Some(Assignee::Agent(logged.by.clone()));
            direct.retain(|_, (i, _)| i != item.as_str());
            if mine {
                direct.insert(logged.by.to_string(), (item.to_string(), *attempt));
            }
        }
    }
    direct.retain(|who, (id, attempt)| {
        state.items[&item(id)]
            .hold
            .as_ref()
            .map(|h| (h.by.as_str(), h.attempt))
            == Some((who.as_str(), *attempt))
    });
    direct
}

/// A person reassigns an item an agent holds from a direct claim, then the
/// tracker restarts. The new tracker keeps the hold, polls no lane for it,
/// and the holder extends it.
async fn reassign_and_restart(fake: &Fake, t: &mut Trackers, m: &mut Model, rng: &mut Rng) {
    direct_and_restart(fake, t, m, rng, true).await;
}

/// A direct claim made for a filter or among several roots, which the
/// assignee rule alone cannot recognise: another agent's poll may ack the
/// item's stale ready record, then the tracker restarts. The hold must
/// survive, with no lane polled for it, and its agent can extend it.
async fn direct_and_restart(
    fake: &Fake,
    t: &mut Trackers,
    m: &mut Model,
    rng: &mut Rng,
    reassign: bool,
) {
    let mut held: Vec<_> = direct_holds(fake)
        .into_iter()
        .filter_map(|(who, claim)| {
            let who = *AGENTS.iter().find(|a| **a == who)?;
            (m.beliefs.get(who) == Some(&claim.0) && m.attempts.get(who) == Some(&claim.1))
                .then_some((who, claim))
        })
        .collect();
    held.sort();
    if held.is_empty() {
        return;
    }
    let (who, (id, attempt)) = held[rng.below(held.len())].clone();
    if reassign {
        let to = AGENTS
            .iter()
            .filter(|a| **a != who)
            .map(|a| Assignee::Agent(key(a)))
            .nth(rng.below(AGENTS.len()));
        let reassigned = WorkRecord::Reassigned {
            item: item(&id),
            to,
        };
        if t.cur.person_record(key("gab"), reassigned).await.is_err() {
            return;
        }
    } else if rng.below(2) == 0 {
        // Another agent's lane poll reaches the item's stale ready record.
        let other = AGENTS.iter().find(|a| **a != who).copied().unwrap();
        claim_step(&t.cur, fake, m, other).await;
        if m.beliefs.get(who) != Some(&id) {
            return;
        }
    }
    let polls = |fake: &Fake| {
        let mine = format!(" {who}");
        fake.calls()
            .iter()
            .filter(|c| c.starts_with("poll ") && c.ends_with(&mine))
            .count()
    };
    let before = polls(fake);
    let new = restart(fake).await;
    t.old = Some(std::mem::replace(&mut t.cur, new));
    let hold = |fake: &Fake| {
        replay(fake).state.items[&item(&id)]
            .hold
            .as_ref()
            .map(|h| (h.by.to_string(), h.attempt))
    };
    assert_eq!(
        hold(fake),
        Some((who.to_owned(), attempt)),
        "a restart ended {who}'s direct claim on {id} after a reassign"
    );
    assert_eq!(
        polls(fake),
        before,
        "a restart polled a lane for {who}'s direct claim"
    );
    let extended = t.cur.extend(&agent(who), &item(&id)).await;
    assert!(
        extended.is_ok(),
        "{who} could not extend its direct claim on {id} after a restart: {extended:?}"
    );
}

/// Whether `who` may act: an agent always, a worker only while its token is
/// live on the tracker serving the call, as the routes check it.
fn may_act(t: &Tracker<Fake>, m: &Model, who: &str) -> bool {
    if !who.contains('-') {
        return true;
    }
    m.tokens
        .get(who)
        .is_some_and(|tok| t.worker_by_token(tok).is_some())
}

/// The supervisor starts a worker. An acknowledged start hands back a token
/// that works, once, under a name nobody had.
async fn worker_start_step(t: &Tracker<Fake>, m: &mut Model, rng: &mut Rng) {
    if m.gone {
        return;
    }
    let name = WORKERS[rng.below(WORKERS.len())];
    let new = NewWorker {
        worker: agent(name),
        vendor: Some("fake".into()),
        profile: Some("fake".into()),
        model: Some("small".into()),
    };
    let sup = TokenHash::of("tok-sup");
    let Ok(reply) = t.start_worker(&agent("sup"), &sup, new).await else {
        return;
    };
    let token = reply["token"].as_str().unwrap().to_owned();
    assert!(
        m.tokens.insert(name, token.clone()).is_none(),
        "{name} was started twice"
    );
    assert!(
        t.worker_by_token(&token).is_some(),
        "{name}'s token is dead"
    );
}

/// The supervisor stops a worker, or the worker says it is done. Either one
/// kills the token and frees what the worker held, with no try spent.
async fn worker_end_step(t: &Tracker<Fake>, m: &mut Model, rng: &mut Rng, finish: bool) {
    let name = WORKERS[rng.below(WORKERS.len())];
    let Some(token) = m.tokens.get(name).cloned() else {
        return;
    };
    let live = t.worker_by_token(&token).is_some();
    let ended = if finish {
        if !live {
            return;
        }
        t.finish_worker(&agent(name)).await
    } else {
        t.stop_worker(Actor::Agent, key("sup"), key(name)).await
    };
    if ended.is_ok() {
        assert!(live, "{name} ended twice");
        assert!(t.worker_by_token(&token).is_none(), "{name}'s token lives");
        m.beliefs.remove(name);
    }
}

/// The supervisor is removed: every worker it has stops and gives back its hold.
async fn supervisor_gone_step(t: &Tracker<Fake>, m: &mut Model) {
    let sup = key("sup");
    if t.stop_workers(|w| w.supervisor == sup).await.is_ok() {
        m.gone = true;
        for name in WORKERS {
            m.beliefs.remove(name);
        }
    }
}

/// One step: maybe a fault, then an operation, then the checks.
async fn step(fake: &Fake, t: &mut Trackers, m: &mut Model, rng: &mut Rng) {
    let mut crash_after = false;
    match rng.below(100) {
        0..=7 => {
            // The tracker dies after some number of its writes.
            fake.fail_publishes_to_after(WORK_EVENTS, rng.below(3) as u32);
            crash_after = true;
        }
        8..=12 => {
            // The old tracker stays up until something fences it.
            let new = restart(fake).await;
            t.old = Some(std::mem::replace(&mut t.cur, new));
        }
        13..=17 => fake.lose_next_publish_reply(WORK_EVENTS),
        // A write that never reached a quorum: a failover cuts it before
        // anyone is told. A record Felix acknowledged is never cut.
        41..=45 => fake.cut_next_lost_write(WORK_EVENTS),
        46..=50 => reassign_and_restart(fake, t, m, rng).await,
        51..=57 => direct_and_restart(fake, t, m, rng, false).await,
        18..=22 => foreign_write(fake, m, rng).await,
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
            pass(&t.cur, Duration::from_secs(10 + rng.below(90) as u64)).await;
            t.cur.sweep().await;
        }
        _ => {}
    }
    let who = CLAIMANTS[rng.below(CLAIMANTS.len())];
    let serving = t.serving(rng.below(100) < 40);
    let mut op = rng.below(26);
    // A worker whose token is dead gets no further than the route's auth.
    if !may_act(serving, m, who) && !(18..=23).contains(&op) {
        op = 23;
    }
    match op {
        18..=19 => worker_start_step(serving, m, rng).await,
        20 => worker_end_step(serving, m, rng, false).await,
        21 => worker_end_step(serving, m, rng, true).await,
        22 if rng.below(4) == 0 => supervisor_gone_step(serving, m).await,
        22 => {}
        // Also where a worker with a dead token lands: it does nothing.
        23 => {
            // Its own generator, so the other steps draw what they always did.
            let mut aux = Rng(m.aux);
            m.aux = aux.next();
            match aux.below(3) {
                0 => needs_step(serving, m, &mut aux).await,
                1 => caps_step(m, &mut aux, who),
                _ => {}
            }
        }
        24..=25 => claim_step(serving, fake, m, who).await,
        0..=1 => seed_item(serving, m).await,
        15 => block_step(serving, m, rng, false).await,
        16..=17 => block_step(serving, m, rng, true).await,
        2..=5 => claim_step(serving, fake, m, who).await,
        14 => filtered_claim_step(serving, fake, m, rng, who).await,
        6..=8 => finish_step(serving, m, rng, who).await,
        9 => extend_step(serving, fake, m, who).await,
        10 => assign_step(serving, m, rng).await,
        11 => ask_step(serving, m, who).await,
        12 => answer_step(serving, fake, rng).await,
        _ => dismiss_step(serving, m, rng).await,
    }
    fake.fail_polls(None);
    // Another writer takes over at the offset the cut freed, racing the
    // tracker's recovery from the lost reply.
    if fake.take_cut().is_some() && rng.below(2) == 0 {
        foreign_write(fake, m, rng).await;
    }
    if crash_after {
        fake.lift_publish_budget();
        t.cur = restart(fake).await;
    }
    m.check(fake);
}

/// Faults stop, a tracker restarts, and the agents drain the queue.
async fn heal(fake: &Fake, m: &mut Model) {
    fake.lift_publish_budget();
    fake.fail_polls(None);
    let t = restart(fake).await;
    // One sweep, with no time passing: whatever a stopped or finished worker
    // still holds goes back now, not when its claim would have run out.
    t.sweep().await;
    for (name, w) in &replay(fake).state.workers {
        if w.stopped_ms.is_some() {
            assert!(
                replay(fake).state.held_by(name.as_str()).is_none(),
                "{name} ended and one sweep left it holding an item"
            );
        }
    }
    // Everyone declares everything, so work held back for want of a
    // capability is worked too.
    for who in CLAIMANTS {
        m.caps
            .insert(who, NEEDS.iter().map(|n| (*n).to_owned()).collect());
    }
    // A person lifts every block, so what was held back is worked too.
    let blocked: Vec<ItemId> = replay(fake)
        .state
        .items
        .values()
        .filter(|i| i.block.is_some())
        .map(|i| i.id.clone())
        .collect();
    for id in blocked {
        let _ = t
            .person_record(key("gab"), WorkRecord::BlockCleared { item: id })
            .await;
    }
    // A person retries an accept that was not acknowledged.
    let mut imported: Vec<_> = m.imported.iter().cloned().collect();
    imported.sort();
    for id in imported {
        if m.accepted.contains(&id) || m.dismissed.contains(&id) {
            continue;
        }
        let accepted = WorkRecord::Accepted {
            item: item(&id),
            tier: Some(2),
            triage_skipped: None,
        };
        if t.person_record(key("gab"), accepted).await.is_ok() {
            m.accepted.insert(id);
        }
    }
    // A poll may only drop a stale record, and a ready record Felix lost track
    // of is published again once it has stood a few visibility periods, so
    // the quiet has to outlast that.
    let mut quiet = 0u32;
    for _ in 0..300 {
        pass(&t, VIS + Duration::from_secs(5)).await;
        // Felix's visibility timeout passes for every delivery nobody settled.
        fake.lapse_all();
        t.sweep().await;
        let mut idle = true;
        for who in CLAIMANTS {
            if !may_act(&t, m, who) {
                continue;
            }
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
        if quiet == RECORD_LAPSE_PERIODS + 2 {
            break;
        }
    }
    let state = replay(fake).state;
    // A worker that ended holds nothing, and a token works exactly while its
    // worker is live, so a restart lost none and revived none.
    for (name, w) in &state.workers {
        if w.stopped_ms.is_some() {
            assert!(
                state.held_by(name.as_str()).is_none(),
                "{name} ended and still holds an item"
            );
        }
    }
    for (name, token) in &m.tokens {
        let live = state.workers[&key(name)].stopped_ms.is_none();
        assert_eq!(t.worker_by_token(token).is_some(), live, "{name}'s token");
    }
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
    // What the workers did across the seeds, so the sweep cannot go quiet.
    let (mut started, mut stopped, mut finished, mut released) = (0, 0, 0, 0);
    let range = seeds();
    let many = range.end - range.start >= 20;
    for seed in range {
        let _replay = Replay(seed);
        let mut rng = Rng(seed);
        let fake = Fake::default();
        // Half the seeds run on a Raft shard, where a refusal burns an offset.
        if seed % 2 == 0 {
            fake.raft_blanks();
        }
        let mut t = Trackers {
            cur: start(&fake).await,
            old: None,
        };
        // Format 24 (needs from 20), so items can be assigned to agents, a question frees the
        // asker, a direct claim says it was one, an item can be blocked, and
        // workers can be started and can finish.
        t.cur
            .person_record(key("gab"), WorkRecord::FormatRaised { to: 24 })
            .await
            .unwrap();
        let vocabulary = WorkRecord::RulesSet {
            target: kbl_core::work::Target {
                source: "github".into(),
                repo: "GetFelix/felix".into(),
            },
            root: None,
            text: None,
            checks: None,
            vocabulary: Some(kbl_core::work::Vocabulary {
                needs: NEEDS.map(str::to_owned).to_vec(),
                ..Default::default()
            }),
        };
        t.cur.person_record(key("gab"), vocabulary).await.unwrap();
        let mut m = Model {
            aux: seed ^ 0xA5A5,
            ..Model::default()
        };
        for _ in 0..STEPS {
            step(&fake, &mut t, &mut m, &mut rng).await;
        }
        heal(&fake, &mut m).await;
        for l in fake.log() {
            match &l.record {
                WorkRecord::WorkerStarted { .. } => started += 1,
                WorkRecord::WorkerStopped { .. } => stopped += 1,
                WorkRecord::WorkerFinished => finished += 1,
                WorkRecord::Released {
                    reason: Some(r), ..
                } if r == kbl_core::work::SUPERVISOR_GONE => released += 1,
                _ => {}
            }
        }
    }
    if many {
        let needy = NEEDY_CLAIMS.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            needy > 0,
            "no claim in the sweep was of work that needs something"
        );
        assert!(
            started > 0 && stopped > 0 && finished > 0 && released > 0,
            "the sweep left workers untouched: {started} started, {stopped} stopped, {finished} finished, {released} released"
        );
    }
}
