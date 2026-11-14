use std::collections::{HashMap, HashSet};
use std::sync::Mutex as StdMutex;

use bytes::Bytes;

use super::*;

const VIS: Duration = Duration::from_secs(90);

#[derive(Clone, PartialEq, Debug)]
enum Slot {
    Pending,
    Leased(String),
    Acked,
}

#[derive(Default)]
struct Net {
    streams: HashMap<String, Vec<(u64, Vec<u8>)>>,
    slots: HashMap<(String, u64), Slot>,
    /// How many times each record has been handed out.
    tries: HashMap<(String, u64), u32>,
    /// Every broker call that changes something, in order.
    calls: Vec<String>,
    fail_publish_on: Option<String>,
    /// Publishes to the stream that still succeed, then every one fails.
    publish_budget: Option<(String, u32)>,
    fail_reads: bool,
    extend_stale: bool,
    /// The writer lease key: holder and version, while it is live.
    lease: Option<(Vec<u8>, u64)>,
    lease_versions: u64,
    /// Polls by this member never answer.
    hang_member: Option<String>,
    /// Members whose connection has dropped: their calls fail until it is forgotten.
    dead: HashSet<String>,
    forgets: Vec<Option<String>>,
    fail_lease: bool,
    gate_extend: bool,
    /// The next publish waits for `open_gate` after the record is on the stream.
    gate_publish: bool,
    /// The next nack waits for `open_gate` before it lands.
    gate_nack: bool,
    /// The next nack panics.
    panic_nack: bool,
    /// Ingest's saved state by name, with versions.
    cursors: HashMap<String, (Vec<u8>, u64)>,
}

/// How many times Felix hands a record out before it is dead-lettered.
const ATTEMPT_BOUND: u32 = 5;

/// An in-memory Felix: streams, and a consumer group per ready stream. A `Fake`
/// is one tracker's connection; `reconnect` is a restarted tracker. Only a
/// member's first poll on a connection reclaims what Felix still holds for it.
#[derive(Clone, Default)]
pub(super) struct Fake {
    net: Arc<StdMutex<Net>>,
    gate: Arc<tokio::sync::Notify>,
    polled: Arc<StdMutex<HashSet<String>>>,
}

impl Fake {
    fn reconnect(&self) -> Fake {
        Fake {
            net: self.net.clone(),
            gate: self.gate.clone(),
            polled: Arc::default(),
        }
    }

    pub(super) fn log(&self) -> Vec<WorkLogged> {
        let net = self.net.lock().unwrap();
        net.streams
            .get(WORK_EVENTS)
            .map(|v| {
                v.iter()
                    .map(|(_, p)| WorkLogged::decode(p).unwrap())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn kinds(&self) -> Vec<String> {
        self.log()
            .iter()
            .map(|l| l.record.kind().to_owned())
            .collect()
    }

    fn calls(&self) -> Vec<String> {
        self.net.lock().unwrap().calls.clone()
    }

    /// The visibility timeout passes for everything leased.
    pub(super) fn lapse_all(&self) {
        for slot in self.net.lock().unwrap().slots.values_mut() {
            if matches!(slot, Slot::Leased(_)) {
                *slot = Slot::Pending;
            }
        }
    }

    /// One delivery's visibility timeout passes.
    fn lapse(&self, stream: &str, offset: u64) {
        self.net
            .lock()
            .unwrap()
            .slots
            .insert((stream.to_owned(), offset), Slot::Pending);
    }

    /// Felix gives up on a record at its attempt bound: it is never handed out again.
    fn dead_letter(&self, stream: &str, offset: u64) {
        self.net
            .lock()
            .unwrap()
            .slots
            .insert((stream.to_owned(), offset), Slot::Acked);
    }

    fn publish_now(&self, stream: &str, payload: Vec<u8>) -> anyhow::Result<(u64, bool)> {
        let mut net = self.net.lock().unwrap();
        if net.fail_publish_on.as_deref() == Some(stream) {
            anyhow::bail!("the broker is down");
        }
        if let Some((s, left)) = net.publish_budget.as_mut()
            && s == stream
        {
            if *left == 0 {
                anyhow::bail!("the broker is down");
            }
            *left -= 1;
        }
        let log = net.streams.entry(stream.to_owned()).or_default();
        let offset = log.len() as u64;
        log.push((offset, payload));
        net.calls.push(format!("publish {stream}"));
        Ok((offset, std::mem::take(&mut net.gate_publish)))
    }

    fn lift_publish_budget(&self) {
        self.net.lock().unwrap().publish_budget = None;
    }

    fn fail_publishes_to_after(&self, stream: &str, ok: u32) {
        self.net.lock().unwrap().publish_budget = Some((stream.to_owned(), ok));
    }

    fn fail_publishes_to(&self, stream: Option<&str>) {
        self.net.lock().unwrap().fail_publish_on = stream.map(str::to_owned);
    }

    /// An agent's connection to the broker drops.
    fn drop_connection(&self, member: &str) {
        self.net.lock().unwrap().dead.insert(member.to_owned());
    }

    fn forgets(&self) -> Vec<Option<String>> {
        self.net.lock().unwrap().forgets.clone()
    }

    fn fail_lease(&self, fail: bool) {
        self.net.lock().unwrap().fail_lease = fail;
    }

    /// The next extend waits for `open_gate`.
    fn hold_extends(&self) {
        self.net.lock().unwrap().gate_extend = true;
    }

    fn hold_next_publish(&self) {
        self.net.lock().unwrap().gate_publish = true;
    }

    fn hold_next_nack(&self) {
        self.net.lock().unwrap().gate_nack = true;
    }

    fn panic_on_next_nack(&self) {
        self.net.lock().unwrap().panic_nack = true;
    }

    fn open_gate(&self) {
        self.gate.notify_one();
    }

    fn fail_reads(&self, fail: bool) {
        self.net.lock().unwrap().fail_reads = fail;
    }

    fn hang(&self, member: Option<&str>) {
        self.net.lock().unwrap().hang_member = member.map(str::to_owned);
    }

    /// The lease key's time to live passes.
    fn expire_lease(&self) {
        self.net.lock().unwrap().lease = None;
    }

    fn lease_is_held(&self) -> bool {
        self.net.lock().unwrap().lease.is_some()
    }

    fn extends_are_stale(&self, stale: bool) {
        self.net.lock().unwrap().extend_stale = stale;
    }

    /// A poll by a tracker that then died before it wrote anything.
    fn poll_and_vanish(&self, stream: &str, member: &str) -> Option<GroupRecord> {
        self.poll_sync(stream, member)
    }

    pub(super) fn ready_count(&self, stream: &str) -> usize {
        self.net
            .lock()
            .unwrap()
            .streams
            .get(stream)
            .map_or(0, Vec::len)
    }

    fn slot(&self, stream: &str, offset: u64) -> Slot {
        let net = self.net.lock().unwrap();
        net.slots
            .get(&(stream.to_owned(), offset))
            .cloned()
            .unwrap_or(Slot::Pending)
    }

    fn poll_sync(&self, stream: &str, member: &str) -> Option<GroupRecord> {
        let reclaim = self.polled.lock().unwrap().insert(member.to_owned());
        let mut net = self.net.lock().unwrap();
        let offsets: Vec<u64> = net
            .streams
            .get(stream)
            .map(|v| v.iter().map(|(o, _)| *o).collect())
            .unwrap_or_default();
        // Felix dead-letters a record at its attempt bound.
        for offset in &offsets {
            let key = (stream.to_owned(), *offset);
            if net.tries.get(&key).copied().unwrap_or(0) >= ATTEMPT_BOUND
                && matches!(net.slots.get(&key), None | Some(Slot::Pending))
            {
                net.slots.insert(key, Slot::Acked);
            }
        }
        let mut pick = None;
        for offset in offsets {
            let key = (stream.to_owned(), offset);
            match net.slots.get(&key).cloned().unwrap_or(Slot::Pending) {
                Slot::Leased(m) if reclaim && m == member => {
                    pick = Some((offset, false));
                    break;
                }
                Slot::Pending if pick.is_none() => {
                    pick = Some((offset, true));
                    // A reclaim looks at the whole stream before it takes new work.
                    if !reclaim {
                        break;
                    }
                }
                _ => {}
            }
        }
        let (offset, fresh) = pick?;
        let key = (stream.to_owned(), offset);
        let tries = net.tries.entry(key.clone()).or_insert(0);
        if fresh {
            *tries += 1;
        }
        let n = *tries;
        net.slots.insert(key, Slot::Leased(member.to_owned()));
        let payload = net.streams[stream]
            .iter()
            .find(|(o, _)| *o == offset)
            .unwrap()
            .1
            .clone();
        Some(GroupRecord {
            offset,
            payload: Bytes::from(payload),
            attempts: n,
            skipped_before: 0,
            publisher: None,
            timestamp_micros: None,
        })
    }
}

impl Broker for Fake {
    async fn publish(&self, stream: &str, payload: Vec<u8>) -> anyhow::Result<u64> {
        let (offset, gated) = self.publish_now(stream, payload)?;
        if gated {
            self.gate.notified().await;
        }
        Ok(offset)
    }

    async fn read_from(&self, stream: &str, from: u64) -> anyhow::Result<Vec<(u64, Vec<u8>)>> {
        let net = self.net.lock().unwrap();
        if net.fail_reads {
            anyhow::bail!("the broker is down");
        }
        Ok(net
            .streams
            .get(stream)
            .map(|v| v.iter().filter(|(o, _)| *o >= from).cloned().collect())
            .unwrap_or_default())
    }

    async fn poll(
        &self,
        stream: &str,
        member: &str,
        _visibility: Duration,
    ) -> anyhow::Result<Option<GroupRecord>> {
        if self.net.lock().unwrap().hang_member.as_deref() == Some(member) {
            std::future::pending::<()>().await;
        }
        if self.net.lock().unwrap().dead.contains(member) {
            anyhow::bail!("the connection dropped");
        }
        let polled = self.poll_sync(stream, member);
        if polled.is_some() {
            self.net
                .lock()
                .unwrap()
                .calls
                .push(format!("poll {stream} {member}"));
        }
        Ok(polled)
    }

    async fn ack(&self, stream: &str, member: &str, offset: u64) -> anyhow::Result<()> {
        let mut net = self.net.lock().unwrap();
        if net.dead.contains(member) {
            anyhow::bail!("the connection dropped");
        }
        net.slots.insert((stream.to_owned(), offset), Slot::Acked);
        net.calls.push(format!("ack {stream}"));
        Ok(())
    }

    async fn nack(&self, stream: &str, member: &str, record: &GroupRecord) -> anyhow::Result<()> {
        // Taken before the panic so the net's lock is not poisoned.
        let panics = std::mem::take(&mut self.net.lock().unwrap().panic_nack);
        assert!(!panics, "the nack panicked");
        let gated = std::mem::take(&mut self.net.lock().unwrap().gate_nack);
        if gated {
            self.gate.notified().await;
        }
        let mut net = self.net.lock().unwrap();
        if net.dead.contains(member) {
            anyhow::bail!("the connection dropped");
        }
        net.slots
            .insert((stream.to_owned(), record.offset), Slot::Pending);
        net.calls.push(format!("nack {stream}"));
        Ok(())
    }

    async fn extend(
        &self,
        stream: &str,
        member: &str,
        record: &GroupRecord,
        by: Duration,
    ) -> Result<Duration, ExtendError> {
        let gated = self.net.lock().unwrap().gate_extend;
        if gated {
            self.gate.notified().await;
            self.net.lock().unwrap().gate_extend = false;
        }
        let net = self.net.lock().unwrap();
        if net.dead.contains(member) {
            return Err(ExtendError::Failed(anyhow::anyhow!(
                "the connection dropped"
            )));
        }
        let mine = net.slots.get(&(stream.to_owned(), record.offset))
            == Some(&Slot::Leased(member.to_owned()));
        if net.extend_stale || !mine {
            return Err(ExtendError::Stale);
        }
        Ok(by)
    }

    async fn lease_put(
        &self,
        holder: Vec<u8>,
        _ttl: Duration,
        expect: Option<u64>,
    ) -> anyhow::Result<Option<u64>> {
        let mut net = self.net.lock().unwrap();
        if net.fail_lease {
            anyhow::bail!("the broker is down");
        }
        let ok = match (&net.lease, expect) {
            (None, None) => true,
            (Some((_, v)), Some(e)) => *v == e,
            _ => false,
        };
        if !ok {
            return Ok(None);
        }
        net.lease_versions += 1;
        let v = net.lease_versions;
        net.lease = Some((holder, v));
        Ok(Some(v))
    }

    async fn cursor_get(&self, name: &str) -> anyhow::Result<Option<(Vec<u8>, u64)>> {
        Ok(self.net.lock().unwrap().cursors.get(name).cloned())
    }

    async fn cursor_put(
        &self,
        name: &str,
        value: Vec<u8>,
        expect: Option<u64>,
    ) -> anyhow::Result<Option<u64>> {
        let mut net = self.net.lock().unwrap();
        let now = net.cursors.get(name).map(|(_, v)| *v);
        if now != expect {
            return Ok(None);
        }
        let next = now.unwrap_or(0) + 1;
        net.cursors.insert(name.to_owned(), (value, next));
        Ok(Some(next))
    }

    async fn forget(&self, member: Option<&str>) {
        let mut net = self.net.lock().unwrap();
        net.forgets.push(member.map(str::to_owned));
        if let Some(m) = member {
            net.dead.remove(m);
            self.polled.lock().unwrap().remove(m);
        }
    }

    async fn close_idle(&self, _idle: Duration) {}
}

pub(super) fn agent(name: &str) -> AgentName {
    AgentName::new(name).unwrap()
}

pub(super) fn item(id: &str) -> ItemId {
    ItemId::new(id).unwrap()
}

pub(super) fn key(k: &str) -> Key {
    Key::new(k).unwrap()
}

/// A person imports an item and accepts it at `tier`.
pub(super) async fn seed(t: &Tracker<Fake>, id: &str, tier: u8) {
    t.person_record(
        key("gab"),
        WorkRecord::Imported {
            item: item(id),
            source: "gh".into(),
            upstream_id: id.into(),
            title: format!("fix {id}"),
        },
    )
    .await
    .unwrap();
    t.person_record(
        key("gab"),
        WorkRecord::Accepted {
            item: item(id),
            tier: Some(tier),
        },
    )
    .await
    .unwrap();
}

pub(super) fn settings() -> Settings {
    Settings {
        grants: Default::default(),
        visibility: VIS,
        lease_ttl: Duration::from_secs(15),
        lease_wait: Duration::ZERO,
        crash_after_poll: false,
        review_tiers: default_review_tiers(),
        cost_floor_micros: 0,
    }
}

/// Time passes, and the tracker renews its lease as it would on its timer.
async fn pass(t: &Tracker<Fake>, total: Duration) {
    let mut left = total;
    while !left.is_zero() {
        let step = left.min(Duration::from_secs(5));
        tokio::time::advance(step).await;
        t.renew_lease().await;
        left -= step;
    }
}

/// A tracker on a new connection. Any earlier tracker's lease has run out by now.
pub(super) async fn start(fake: &Fake) -> Tracker<Fake> {
    start_with(fake, settings()).await
}

pub(super) async fn start_with(fake: &Fake, settings: Settings) -> Tracker<Fake> {
    fake.expire_lease();
    Tracker::start(fake.reconnect(), settings).await.unwrap()
}

pub(super) async fn claim(t: &Tracker<Fake>, who: &str) -> Value {
    t.next(&agent(who), Duration::ZERO).await.unwrap()
}

pub(super) fn claimed_id(v: &Value) -> Option<String> {
    v["claimed"]
        .as_bool()
        .unwrap()
        .then(|| v["item"]["id"].as_str().unwrap().to_owned())
}

#[tokio::test]
async fn a_ready_item_is_published_to_its_tier() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 1).await;
    assert_eq!(fake.ready_count("work.ready.p1"), 1);
    assert_eq!(fake.ready_count("work.ready.p2"), 0);
}

#[tokio::test]
async fn tiers_are_polled_in_order() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "low", 3).await;
    seed(&t, "mid", 2).await;
    seed(&t, "top", 0).await;
    let order: Vec<_> = [claim(&t, "w1").await, claim(&t, "w2").await]
        .iter()
        .map(|v| claimed_id(v).unwrap())
        .collect();
    assert_eq!(order, ["top", "mid"]);
}

#[tokio::test]
async fn two_agents_racing_for_one_item_get_it_once() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "only", 2).await;
    let (a, b) = tokio::join!(claim(&t, "w1"), claim(&t, "w2"));
    let winners: Vec<_> = [a, b].iter().filter_map(claimed_id).collect();
    assert_eq!(winners, ["only"]);
    assert_eq!(fake.kinds().iter().filter(|k| *k == "claimed").count(), 1);
}

#[tokio::test]
async fn the_claimed_record_is_written_before_the_answer_and_names_the_agent() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    let reply = claim(&t, "w3").await;
    assert_eq!(claimed_id(&reply).as_deref(), Some("a1"));
    let log = fake.log();
    let c = log.last().unwrap();
    assert_eq!(c.record.kind(), "claimed");
    assert_eq!(c.by.as_str(), "w3");
    assert_eq!(c.actor, Actor::Agent);
}

#[tokio::test]
async fn a_claim_whose_record_cannot_be_written_is_not_held() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    fake.fail_publishes_to(Some(WORK_EVENTS));
    assert!(t.next(&agent("w1"), Duration::ZERO).await.is_err());
    fake.fail_publishes_to(None);
    // The delivery went back, so the next agent gets the item and it is logged once.
    let reply = claim(&t, "w2").await;
    assert_eq!(claimed_id(&reply).as_deref(), Some("a1"));
    let claims: Vec<_> = fake
        .log()
        .into_iter()
        .filter(|l| l.record.kind() == "claimed")
        .collect();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].by.as_str(), "w2");
}

#[tokio::test]
async fn done_is_written_before_the_ack() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    let reply = t.complete(&agent("w1"), &item("a1"), 1234).await.unwrap();
    assert_eq!(reply["status"], "done");
    let calls = fake.calls();
    let done = calls
        .iter()
        .rposition(|c| c == "publish work.events")
        .unwrap();
    let ack = calls.iter().position(|c| c == "ack work.ready.p2").unwrap();
    assert!(done < ack, "{calls:?}");
    let log = fake.log();
    let WorkRecord::Done { cost_micros, .. } = &log.last().unwrap().record else {
        panic!("the last record is not done");
    };
    assert_eq!(*cost_micros, 1234);
}

/// The cost each record of `kind` on the log carries.
fn costs(fake: &Fake, kind: &str) -> Vec<u64> {
    fake.log()
        .iter()
        .filter_map(|l| match &l.record {
            WorkRecord::Done { cost_micros, .. }
            | WorkRecord::Released { cost_micros, .. }
            | WorkRecord::GaveUp { cost_micros, .. }
                if l.record.kind() == kind =>
            {
                Some(*cost_micros)
            }
            _ => None,
        })
        .collect()
}

fn floor(micros: u64) -> Settings {
    Settings {
        cost_floor_micros: micros,
        ..settings()
    }
}

#[tokio::test]
async fn a_claim_is_charged_at_least_the_floor() {
    let fake = Fake::default();
    let t = start_with(&fake, floor(500)).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    t.release(&agent("w1"), &item("a1"), None, 0).await.unwrap();
    claim(&t, "w1").await;
    t.give_up(&agent("w1"), &item("a1"), "stuck".into(), 7)
        .await
        .unwrap();
    assert_eq!(costs(&fake, "released"), [500]);
    assert_eq!(costs(&fake, "gave_up"), [500]);
    // Done reports the charged cost back. A report above the floor stands.
    seed(&t, "a2", 2).await;
    claim(&t, "w1").await;
    let reply = t.complete(&agent("w1"), &item("a2"), 0).await.unwrap();
    assert_eq!(reply["cost_micros"], 500);
    seed(&t, "a3", 2).await;
    claim(&t, "w1").await;
    t.complete(&agent("w1"), &item("a3"), 1234).await.unwrap();
    assert_eq!(costs(&fake, "done"), [500, 1234]);
    seed(&t, "a4", 2).await;
    let core = t.0.core.lock().await;
    let left = |id: &str| core.state.budgets[&item(id)].cost_left_micros;
    assert_eq!(left("a1") + 1000, left("a4"));
    assert_eq!(left("a2") + 500, left("a4"));
}

#[tokio::test]
async fn a_claim_found_lapsed_by_the_next_poll_is_charged_the_floor() {
    let fake = Fake::default();
    let t = start_with(&fake, floor(500)).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    fake.lapse_all();
    claim(&t, "w2").await;
    assert_eq!(costs(&fake, "released"), [500]);
}

#[tokio::test(start_paused = true)]
async fn a_lapsed_claim_is_charged_the_floor() {
    let fake = Fake::default();
    let t = start_with(&fake, floor(500)).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    pass(&t, VIS + Duration::from_secs(1)).await;
    t.sweep().await;
    assert_eq!(costs(&fake, "released"), [500]);
}

#[tokio::test]
async fn a_dead_agents_item_goes_to_another_after_visibility() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    // Nothing for w2 while w1's claim stands.
    assert_eq!(claimed_id(&claim(&t, "w2").await), None);
    fake.lapse_all();
    let reply = claim(&t, "w2").await;
    assert_eq!(claimed_id(&reply).as_deref(), Some("a1"));
    assert_eq!(reply["attempt"], 1);
    // The reply says which branch links a pull request to the item.
    assert_eq!(reply["item"]["pr_branch"], "kbl/a1");
    let log = fake.log();
    let tail: Vec<_> = log[log.len() - 2..]
        .iter()
        .map(|l| (l.record.kind(), l.actor, l.by.as_str()))
        .collect();
    assert_eq!(
        tail,
        [
            ("released", Actor::Tracker, "tracker"),
            ("claimed", Actor::Agent, "w2")
        ]
    );
    // The dead agent, back, finds it lost.
    assert!(matches!(
        t.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Lost(_))
    ));
    // Finishing after a takeover is a lost claim too, same as extending.
    assert!(matches!(
        t.complete(&agent("w1"), &item("a1"), 0).await,
        Err(Error::Lost(_))
    ));
}

#[tokio::test]
async fn an_item_the_agent_released_is_handed_out_again() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    t.release(&agent("w1"), &item("a1"), Some("stuck".into()), 5)
        .await
        .unwrap();
    assert!(fake.calls().contains(&"nack work.ready.p2".to_owned()));
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
    // The second release uses the item's second try; the item then waits for a person.
    t.release(&agent("w2"), &item("a1"), None, 0).await.unwrap();
    assert_eq!(claimed_id(&claim(&t, "w1").await), None);
    assert!(fake.calls().contains(&"ack work.ready.p2".to_owned()));
}

#[tokio::test]
async fn extend_reports_a_lapsed_claim_and_drops_it() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    assert!(t.extend(&agent("w1"), &item("a1")).await.is_ok());
    fake.extends_are_stale(true);
    assert!(matches!(
        t.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Lost(_))
    ));
    // The tracker no longer treats w1 as holding a delivery, so w1 may ask for work again.
    fake.extends_are_stale(false);
    assert!(t.next(&agent("w1"), Duration::ZERO).await.is_ok());
}

#[tokio::test]
async fn a_stale_ready_record_is_acked_and_skipped() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "gone", 2).await;
    seed(&t, "kept", 3).await;
    // The item is closed after it was published ready.
    t.person_record(
        key("gab"),
        WorkRecord::Closed {
            item: item("gone"),
            why: kbl_core::work::CloseWhy::Dismissed,
        },
    )
    .await
    .unwrap();
    let reply = claim(&t, "w1").await;
    assert_eq!(claimed_id(&reply).as_deref(), Some("kept"));
    assert!(fake.calls().contains(&"ack work.ready.p2".to_owned()));
    assert!(!fake.kinds().contains(&"released".to_owned()));
}

#[tokio::test]
async fn a_restart_between_poll_and_log_does_not_hold_the_item() {
    let fake = Fake::default();
    let first = start(&fake).await;
    seed(&first, "a1", 2).await;
    // The first tracker polled for w1 and died before it wrote `claimed`.
    assert!(fake.poll_and_vanish("work.ready.p2", "w1").is_some());
    drop(first);

    // A real restart takes far longer than this; stamps are in microseconds.
    std::thread::sleep(Duration::from_millis(2));
    let second = start(&fake).await;
    // Nothing was logged for the item, so the restarted tracker offers it again.
    assert_eq!(fake.kinds().iter().filter(|k| *k == "claimed").count(), 0);
    assert_eq!(
        claimed_id(&claim(&second, "w2").await).as_deref(),
        Some("a1")
    );
    fake.lapse("work.ready.p2", 0);
    // w1's stranded delivery comes back as a leftover and is skipped, not claimed again.
    assert_eq!(claimed_id(&claim(&second, "w1").await), None);
    assert_eq!(fake.kinds().iter().filter(|k| *k == "claimed").count(), 1);
}

#[tokio::test]
async fn the_work_next_reply_carries_the_work_loop() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    let got = claim(&t, "w1").await;
    let text = got["loop"].as_str().unwrap();
    assert!(
        text.contains("work_next again") && text.contains("asked to stop"),
        "{text}"
    );
    t.complete(&agent("w1"), &item("a1"), 0).await.unwrap();
    let empty = claim(&t, "w1").await;
    assert_eq!(empty["claimed"], false);
    assert!(empty["loop"].as_str().unwrap().contains("End your session"));
}

#[tokio::test]
async fn an_agent_that_holds_a_claim_gets_it_back_from_work_next() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    let again = claim(&t, "w1").await;
    assert_eq!(again["resumed"], true);
    assert_eq!(claimed_id(&again).as_deref(), Some("a1"));
    assert_eq!(fake.kinds().iter().filter(|k| *k == "claimed").count(), 1);
}

#[tokio::test]
async fn a_restart_takes_back_the_deliveries_felix_still_holds_for_each_agent() {
    let fake = Fake::default();
    let first = start(&fake).await;
    seed(&first, "a1", 2).await;
    seed(&first, "a2", 2).await;
    seed(&first, "a3", 2).await;
    claim(&first, "w1").await;
    claim(&first, "w2").await;
    drop(first);

    let second = start(&fake).await;
    // Each agent's delivery is back, so its claim can be extended and finished.
    let r = second.extend(&agent("w1"), &item("a1")).await;
    assert!(r.is_ok(), "{r:?}");
    assert!(second.extend(&agent("w2"), &item("a2")).await.is_ok());
    // An agent asking for work gets its own item, never a second one.
    let again = claim(&second, "w2").await;
    assert_eq!(
        (again["resumed"].as_bool(), claimed_id(&again).as_deref()),
        (Some(true), Some("a2"))
    );
    assert_eq!(fake.kinds().iter().filter(|k| *k == "claimed").count(), 2);
    second.complete(&agent("w1"), &item("a1"), 0).await.unwrap();
    assert_eq!(fake.slot("work.ready.p2", 0), Slot::Acked);
    // The third item is still there for whoever asks.
    assert_eq!(
        claimed_id(&claim(&second, "w3").await).as_deref(),
        Some("a3")
    );
}

#[tokio::test]
async fn a_hold_whose_delivery_is_gone_after_a_restart_is_released_not_resumed() {
    let fake = Fake::default();
    let first = start(&fake).await;
    seed(&first, "a1", 2).await;
    seed(&first, "a2", 2).await;
    claim(&first, "w1").await;
    drop(first);
    // Felix gave up on the delivery while the tracker was down, and a tracker of
    // a later generation has handed it to another member.
    fake.lapse("work.ready.p2", 0);
    assert!(fake.poll_and_vanish("work.ready.p2", "w9").is_some());

    let second = start(&fake).await;
    // The hold is freed at startup, without another agent polling.
    let log = fake.log();
    let freed = log
        .iter()
        .rev()
        .find(|l| l.record.kind() == "released")
        .unwrap();
    assert_eq!(
        (freed.actor, freed.by.as_str()),
        (Actor::Tracker, "tracker")
    );
    assert!(matches!(
        second.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Lost(_)) | Err(Error::NotHeld(_))
    ));
    // w1 is not told it still holds a1.
    let reply = claim(&second, "w1").await;
    assert_ne!(reply["resumed"], true);
}

#[tokio::test(start_paused = true)]
async fn a_hold_that_is_not_extended_is_released_by_the_sweep() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    t.sweep().await;
    assert_eq!(fake.kinds().iter().filter(|k| *k == "released").count(), 0);
    pass(&t, VIS + Duration::from_secs(1)).await;
    t.sweep().await;
    // Freed with no other agent polling.
    let log = fake.log();
    let freed = log.last().unwrap();
    assert_eq!(freed.record.kind(), "released");
    assert_eq!(freed.actor, Actor::Tracker);
    assert!(matches!(
        t.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Lost(_))
    ));
    // Once Felix hands the delivery on, anyone can take the now-ready item.
    fake.lapse("work.ready.p2", 0);
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
}

#[tokio::test(start_paused = true)]
async fn an_extended_hold_is_kept_by_the_sweep() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    for _ in 0..4 {
        pass(&t, Duration::from_secs(30)).await;
        t.extend(&agent("w1"), &item("a1")).await.unwrap();
        t.sweep().await;
    }
    assert_eq!(fake.kinds().iter().filter(|k| *k == "released").count(), 0);
}

#[tokio::test]
async fn a_stale_holders_late_calls_cannot_settle_the_next_holders_delivery() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    // w1 hangs. A person takes the item back, Felix hands the delivery on, and w2 claims.
    t.person_record(
        key("gab"),
        WorkRecord::Released {
            item: item("a1"),
            attempt: 1,
            reason: None,
            cost_micros: 0,
        },
    )
    .await
    .unwrap();
    fake.lapse("work.ready.p2", 0);
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
    let acks_before = fake.calls().iter().filter(|c| c.starts_with("ack")).count();
    // w1 comes back. None of its calls may touch w2's delivery.
    assert!(matches!(
        t.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Lost(_))
    ));
    assert!(matches!(
        t.complete(&agent("w1"), &item("a1"), 0).await,
        Err(Error::Lost(_))
    ));
    assert!(matches!(
        t.release(&agent("w1"), &item("a1"), None, 0).await,
        Err(Error::Lost(_))
    ));
    assert_eq!(fake.slot("work.ready.p2", 0), Slot::Leased("w2".into()));
    let acks_after = fake.calls().iter().filter(|c| c.starts_with("ack")).count();
    assert_eq!(acks_before, acks_after);
    // And w2 can still finish.
    t.complete(&agent("w2"), &item("a1"), 0).await.unwrap();
    assert_eq!(fake.slot("work.ready.p2", 0), Slot::Acked);
}

#[tokio::test]
async fn a_failed_reload_does_not_empty_the_state_and_nothing_is_served_until_it_works() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    fake.fail_publishes_to(Some(WORK_EVENTS));
    assert!(t.complete(&agent("w1"), &item("a1"), 0).await.is_err());
    fake.fail_publishes_to(None);
    fake.fail_reads(true);
    // The state must be rebuilt, the read fails: refuse rather than serve an empty board.
    assert!(matches!(
        t.next(&agent("w2"), Duration::ZERO).await,
        Err(Error::Unavailable(_))
    ));
    assert!(matches!(
        t.complete(&agent("w1"), &item("a1"), 0).await,
        Err(Error::Unavailable(_))
    ));
    fake.fail_reads(false);
    // Rebuilt from the log: the item is still w1's, and finishes once.
    t.complete(&agent("w1"), &item("a1"), 0).await.unwrap();
    assert_eq!(fake.kinds().iter().filter(|k| *k == "done").count(), 1);
}

#[tokio::test(start_paused = true)]
async fn one_hung_felix_call_stalls_only_that_agent() {
    // The outer limit makes a missing timeout fail the test instead of hanging it.
    tokio::time::timeout(Duration::from_secs(60), async {
        let fake = Fake::default();
        let t = start(&fake).await;
        seed(&t, "a1", 2).await;
        seed(&t, "a2", 2).await;
        claim(&t, "w3").await;
        fake.hang(Some("w1"));
        let hung = tokio::spawn({
            let t = t.clone();
            async move { t.next(&agent("w1"), Duration::ZERO).await }
        });
        tokio::task::yield_now().await;
        // Others carry on while w1's poll hangs.
        assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a2"));
        t.complete(&agent("w3"), &item("a1"), 0).await.unwrap();
        seed(&t, "a3", 2).await;
        // The hung call ends by its own timeout, and its connection is dropped.
        assert!(matches!(hung.await.unwrap(), Err(Error::Felix(_))));
        assert!(fake.forgets().contains(&Some("w1".to_owned())));
    })
    .await
    .expect("a hung Felix call stalled the tracker");
}

#[tokio::test]
async fn a_dropped_connection_does_not_cost_a_live_agent_its_claim() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    fake.drop_connection("w1");
    // The extend reconnects within the same call and the claim stands.
    assert!(t.extend(&agent("w1"), &item("a1")).await.is_ok());
    assert!(fake.forgets().contains(&Some("w1".to_owned())));
    assert_eq!(fake.slot("work.ready.p2", 0), Slot::Leased("w1".into()));
    // Settling reconnects too.
    fake.drop_connection("w1");
    t.complete(&agent("w1"), &item("a1"), 0).await.unwrap();
    assert_eq!(fake.slot("work.ready.p2", 0), Slot::Acked);
    assert_eq!(fake.kinds().iter().filter(|k| *k == "released").count(), 0);
}

#[tokio::test(start_paused = true)]
async fn the_lease_must_outlast_a_write_before_the_tracker_writes() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    // The lease cannot be renewed, and what is left is less than a write may take.
    fake.fail_lease(true);
    tokio::time::advance(Duration::from_secs(9)).await;
    assert!(matches!(
        t.person_record(
            key("gab"),
            WorkRecord::Accepted {
                item: item("a1"),
                tier: Some(1),
            },
        )
        .await,
        Err(Error::Unavailable(_))
    ));
    // Once renewal works the tracker renews first and writes.
    fake.fail_lease(false);
    assert!(t.next(&agent("w1"), Duration::ZERO).await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn an_extend_that_lands_after_the_sweep_freed_the_hold_is_lost() {
    let fake = Fake::default();
    fake.expire_lease();
    let t = Tracker::start(
        fake.reconnect(),
        Settings {
            visibility: Duration::from_secs(5),
            ..settings()
        },
    )
    .await
    .unwrap();
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    fake.hold_extends();
    let extending = tokio::spawn({
        let t = t.clone();
        async move { t.extend(&agent("w1"), &item("a1")).await }
    });
    tokio::task::yield_now().await;
    // While Felix is still answering, the hold lapses and the sweep frees it.
    pass(&t, Duration::from_secs(6)).await;
    t.sweep().await;
    assert_eq!(fake.kinds().last().map(String::as_str), Some("released"));
    fake.open_gate();
    let r = extending.await.unwrap();
    assert!(matches!(r, Err(Error::Lost(_))), "{r:?}");
}

#[tokio::test]
async fn a_second_tracker_cannot_start_while_the_first_holds_the_lease() {
    let fake = Fake::default();
    let _first = start(&fake).await;
    let second = Tracker::start(fake.reconnect(), settings()).await;
    assert!(second.is_err());
}

#[tokio::test]
async fn a_tracker_that_lost_the_lease_serves_and_writes_nothing() {
    let fake = Fake::default();
    let first = start(&fake).await;
    seed(&first, "a1", 2).await;
    claim(&first, "w1").await;
    // The lease runs out and another tracker takes it.
    let second = start(&fake).await;
    first.renew_lease().await;
    let before = fake.log().len();
    assert!(matches!(
        first.complete(&agent("w1"), &item("a1"), 0).await,
        Err(Error::Unavailable(_))
    ));
    assert!(matches!(
        first
            .person_record(
                key("gab"),
                WorkRecord::Imported {
                    item: item("b1"),
                    source: "gh".into(),
                    upstream_id: "b1".into(),
                    title: "t".into(),
                },
            )
            .await,
        Err(Error::Unavailable(_))
    ));
    assert!(matches!(
        first.next(&agent("w2"), Duration::ZERO).await,
        Err(Error::Unavailable(_))
    ));
    assert_eq!(fake.log().len(), before);
    // The one with the lease serves.
    assert!(second.next(&agent("w2"), Duration::ZERO).await.is_ok());
}

#[tokio::test]
async fn an_item_over_budget_leaves_ready_and_is_not_published_again() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    t.person_record(
        key("gab"),
        WorkRecord::BudgetSet {
            root: item("a1"),
            items_left: 5,
            max_depth: 3,
            cost_left_micros: 0,
            redos_per_item: 3,
        },
    )
    .await
    .unwrap();
    assert_eq!(claimed_id(&claim(&t, "w1").await), None);
    assert!(fake.kinds().contains(&"refused".to_owned()));
    let published = fake.ready_count("work.ready.p2");
    for _ in 0..3 {
        assert_eq!(claimed_id(&claim(&t, "w2").await), None);
    }
    assert_eq!(fake.ready_count("work.ready.p2"), published);
    assert_eq!(fake.kinds().iter().filter(|k| *k == "refused").count(), 1);
}

#[tokio::test]
async fn a_person_closing_a_held_item_ends_the_agents_claim_and_settles_the_delivery() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    t.person_record(
        key("gab"),
        WorkRecord::Closed {
            item: item("a1"),
            why: kbl_core::work::CloseWhy::Dismissed,
        },
    )
    .await
    .unwrap();
    // The claim must not be extended for a hold the log no longer shows.
    assert!(matches!(
        t.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Lost(_))
    ));
    assert_eq!(fake.slot("work.ready.p2", 0), Slot::Acked);
    assert!(matches!(
        t.complete(&agent("w1"), &item("a1"), 0).await,
        Err(Error::Lost(_))
    ));
    assert_eq!(fake.kinds().iter().filter(|k| *k == "done").count(), 0);
    // And w1 is free to take other work.
    seed(&t, "a2", 2).await;
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a2"));
}

#[tokio::test]
async fn a_closed_attempt_ends_the_claim_too() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    t.person_record(
        key("gab"),
        WorkRecord::AttemptClosed {
            item: item("a1"),
            attempt: 1,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        t.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Lost(_))
    ));
    assert_eq!(fake.slot("work.ready.p2", 0), Slot::Acked);
}

#[tokio::test]
async fn a_failed_write_is_rebuilt_from_the_log_not_trusted() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    fake.fail_publishes_to(Some(WORK_EVENTS));
    // The fold accepted `done` in memory, but the write failed.
    assert!(t.complete(&agent("w1"), &item("a1"), 0).await.is_err());
    fake.fail_publishes_to(None);
    // The next call reloads, so the item is still held and can be finished.
    assert!(t.complete(&agent("w1"), &item("a1"), 0).await.is_ok());
    assert_eq!(fake.kinds().iter().filter(|k| *k == "done").count(), 1);
}

#[tokio::test]
async fn a_record_the_fold_refuses_is_not_appended() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    let before = fake.log().len();
    // An agent that holds nothing cannot finish the item.
    let err = t.complete(&agent("w1"), &item("a1"), 0).await.unwrap_err();
    assert!(matches!(err, Error::NotHeld(_)));
    // A person cannot write an agent's record.
    let err = t
        .person_record(
            key("gab"),
            WorkRecord::Claimed {
                item: item("a1"),
                attempt: 1,
                deadline_ms: 0,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Refused { ref why } if why == "actor_not_allowed"),
        "{err}"
    );
    assert_eq!(fake.log().len(), before);
}

#[tokio::test]
async fn ingest_writes_upstream_records_as_the_tracker_under_its_own_name() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let imported = WorkRecord::Imported {
        item: item("r1"),
        source: "github".into(),
        upstream_id: "GetFelix/felix#1".into(),
        title: "a bug".into(),
    };
    t.ingest_record(key("ingest-gh"), imported.clone())
        .await
        .unwrap();
    // The same upstream id again is the fold's duplicate, not a second root.
    let again = WorkRecord::Imported {
        item: item("r2"),
        source: "github".into(),
        upstream_id: "GetFelix/felix#1".into(),
        title: "a bug".into(),
    };
    let err = t.ingest_record(key("ingest-gh"), again).await.unwrap_err();
    assert!(matches!(err, Error::Refused { why } if why == "duplicate"));
    let log = fake.log();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].by, key("ingest-gh"));
    assert_eq!(log[0].actor, Actor::Tracker);
    let roots = t.ingest_roots().await.unwrap();
    assert_eq!(roots["roots"][0]["upstream_id"], "GetFelix/felix#1");
    assert_eq!(roots["roots"][0]["closed"], false);
}

#[tokio::test]
async fn ingest_cannot_write_what_a_person_or_an_agent_writes() {
    let fake = Fake::default();
    let t = start(&fake).await;
    for record in [
        WorkRecord::BudgetSet {
            root: item("r1"),
            items_left: 99,
            max_depth: 9,
            cost_left_micros: 1,
            redos_per_item: 9,
        },
        WorkRecord::Rejected { item: item("r1") },
        WorkRecord::Closed {
            item: item("r1"),
            why: kbl_core::work::CloseWhy::Dismissed,
        },
    ] {
        let err = t.ingest_record(key("ingest-gh"), record).await.unwrap_err();
        assert!(matches!(err, Error::Refused { why } if why == "not_allowed"));
    }
    assert!(fake.log().is_empty());
}

#[tokio::test]
async fn ingest_cannot_touch_an_item_it_did_not_import() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let ingest = |r| {
        let t = &t;
        async move { t.ingest_record(key("ingest-gh"), r).await }
    };
    // `mine` is imported by ingest; `theirs` is a person's own item.
    ingest(WorkRecord::Imported {
        item: item("mine"),
        source: "github".into(),
        upstream_id: "o/r#1".into(),
        title: "a bug".into(),
    })
    .await
    .unwrap();
    t.person_record(
        key("gab"),
        WorkRecord::Proposed {
            item: item("theirs"),
            parent: None,
            key: key("fix"),
            work: kbl_core::work::WorkKind::Fix,
            title: "mine to run".into(),
            review: None,
        },
    )
    .await
    .unwrap();
    let before = fake.log().len();
    for record in [
        WorkRecord::Accepted {
            item: item("theirs"),
            tier: Some(0),
        },
        WorkRecord::Closed {
            item: item("theirs"),
            why: kbl_core::work::CloseWhy::Upstream,
        },
        WorkRecord::Reopened {
            item: item("theirs"),
        },
        WorkRecord::PrStatus {
            item: item("theirs"),
            status: kbl_core::work::PrSnapshot {
                pr: "o/r#2".into(),
                phase: kbl_core::work::PrPhase::Merged,
                review: kbl_core::work::PrReview::Approved,
                checks: kbl_core::work::PrChecks::Green,
                failing: None,
            },
        },
        WorkRecord::Linked {
            item: item("mine"),
            to: item("theirs"),
        },
        WorkRecord::Linked {
            item: item("theirs"),
            to: item("mine"),
        },
        WorkRecord::Context {
            item: item("theirs"),
            entry: key("note"),
            text: kbl_core::content::Content::Text("hi".into()),
        },
        WorkRecord::Proposed {
            item: item("x"),
            parent: Some(item("theirs")),
            key: key("triage"),
            work: kbl_core::work::WorkKind::Triage,
            title: "t".into(),
            review: None,
        },
    ] {
        let err = ingest(record).await.unwrap_err();
        assert!(
            matches!(err, Error::Refused { ref why } if why == "not_imported"),
            "{err}"
        );
    }
    assert_eq!(fake.log().len(), before);
    // Its own root still works, and a too-long inline text does not.
    ingest(WorkRecord::Context {
        item: item("mine"),
        entry: key("note"),
        text: kbl_core::content::Content::Text("ok".into()),
    })
    .await
    .unwrap();
    let err = ingest(WorkRecord::Context {
        item: item("mine"),
        entry: key("big"),
        text: kbl_core::content::Content::Text("x".repeat(5000)),
    })
    .await
    .unwrap_err();
    assert!(matches!(err, Error::Refused { why } if why == "too_big"));
}

#[tokio::test]
async fn ingest_state_is_kept_with_a_version_and_a_stale_save_is_refused() {
    let fake = Fake::default();
    let t = start(&fake).await;
    assert_eq!(
        t.ingest_state("github").await.unwrap()["state"],
        Value::Null
    );
    let v1 = t
        .ingest_save("github", &json!({"since": "a"}), None)
        .await
        .unwrap();
    assert_eq!(v1["version"], 1);
    // A restarted tracker still has it.
    fake.expire_lease();
    let t2 = Tracker::start(fake.reconnect(), settings()).await.unwrap();
    let got = t2.ingest_state("github").await.unwrap();
    assert_eq!(got["state"], json!({"since": "a"}));
    // Saving at the version it read goes through; saving at an old one does not.
    t2.ingest_save("github", &json!({"since": "b"}), Some(1))
        .await
        .unwrap();
    let err = t2
        .ingest_save("github", &json!({"since": "c"}), Some(1))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Refused { why } if why == "stale_state"));
}

#[tokio::test]
async fn ingest_cannot_close_as_merged_without_a_merged_status() {
    let fake = Fake::default();
    let t = start(&fake).await;
    t.ingest_record(
        key("ingest-gh"),
        WorkRecord::Imported {
            item: item("mine"),
            source: "github".into(),
            upstream_id: "o/r#1".into(),
            title: "a bug".into(),
        },
    )
    .await
    .unwrap();
    let err = t
        .ingest_record(
            key("ingest-gh"),
            WorkRecord::Closed {
                item: item("mine"),
                why: kbl_core::work::CloseWhy::Merged,
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Refused { ref why } if why == "not_merged"),
        "{err}"
    );
    assert_eq!(fake.log().len(), 1);
}

pub(super) async fn seed_in_felix(t: &Tracker<Fake>, id: &str) {
    t.person_record(
        key("gab"),
        WorkRecord::Imported {
            item: item(id),
            source: "github".into(),
            upstream_id: format!("GetFelix/felix#{id}"),
            title: format!("fix {id}"),
        },
    )
    .await
    .unwrap();
    t.person_record(
        key("gab"),
        WorkRecord::Accepted {
            item: item(id),
            tier: Some(2),
        },
    )
    .await
    .unwrap();
}

fn felix_rules(root: Option<&str>, text: Option<&str>, checks: Option<&[&str]>) -> WorkRecord {
    WorkRecord::RulesSet {
        target: kbl_core::work::Target {
            source: "github".into(),
            repo: "GetFelix/felix".into(),
        },
        root: root.map(item),
        text: text.map(str::to_owned),
        checks: checks.map(|c| c.iter().map(|s| s.to_string()).collect()),
    }
}

#[tokio::test]
async fn a_claim_carries_the_latest_rules_and_the_root_overrides_the_repo() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed_in_felix(&t, "a1").await;
    let first = t
        .person_record(key("gab"), felix_rules(None, Some("old"), None))
        .await
        .unwrap();
    let v = t
        .person_record(
            key("gab"),
            felix_rules(None, Some("one worktree"), Some(&["task lint"])),
        )
        .await
        .unwrap();
    assert_ne!(first["offset"], v["offset"]);

    let reply = claim(&t, "w1").await;
    assert_eq!(reply["rules"]["text"], "one worktree");
    assert_eq!(reply["rules"]["checks"], json!(["task lint"]));
    assert_eq!(reply["rules"]["version"], v["offset"]);

    // The root keeps the repo's text and replaces its checks; a resumed claim shows it.
    let r = t
        .person_record(
            key("gab"),
            felix_rules(Some("a1"), None, Some(&["cargo test"])),
        )
        .await
        .unwrap();
    let again = claim(&t, "w1").await;
    assert_eq!(again["resumed"], true);
    assert_eq!(again["rules"]["text"], "one worktree");
    assert_eq!(again["rules"]["checks"], json!(["cargo test"]));
    assert_eq!(again["rules"]["version"], r["offset"]);
    assert_eq!(again["rules"]["repo_version"], v["offset"]);
}

#[tokio::test]
async fn an_agent_and_ingest_cannot_write_rules() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed_in_felix(&t, "a1").await;
    let err = t
        .ingest_record(key("ingest-gh"), felix_rules(None, Some("x"), None))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Refused { why } if why == "not_allowed"));
    assert!(claim(&t, "w1").await["rules"].is_null());
    assert!(!fake.kinds().contains(&"rules_set".to_owned()));
}

#[tokio::test]
async fn rules_over_the_size_bounds_are_refused() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed_in_felix(&t, "a1").await;
    let big = "x".repeat(kbl_core::work::MAX_RULES_TEXT + 1);
    let err = t
        .person_record(key("gab"), felix_rules(None, Some(&big), None))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)));
    let many = vec!["c"; kbl_core::work::MAX_RULE_CHECKS + 1];
    let err = t
        .person_record(key("gab"), felix_rules(None, None, Some(&many)))
        .await
        .unwrap_err();
    assert!(!matches!(err, Error::Felix(_)));
    assert!(!fake.kinds().contains(&"rules_set".to_owned()));
}

#[tokio::test]
async fn rules_read_answers_only_the_holder_and_matches_the_claim() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed_in_felix(&t, "a1").await;
    t.person_record(
        key("gab"),
        felix_rules(None, Some("one worktree"), Some(&["task lint"])),
    )
    .await
    .unwrap();
    let err = t.rules_read(&agent("w2"), &item("a1")).await.unwrap_err();
    assert!(matches!(err, Error::NotHeld(_)));
    let claimed = claim(&t, "w1").await;
    let read = t.rules_read(&agent("w1"), &item("a1")).await.unwrap();
    assert_eq!(read["rules"], claimed["rules"]);
}

mod races;
mod review;

#[tokio::test(start_paused = true)]
async fn a_dead_lettered_ready_record_is_published_again() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    fake.dead_letter("work.ready.p2", 0);
    assert_eq!(claimed_id(&claim(&t, "w1").await), None);
    // The tracker cannot see a dead letter, so it publishes again after the record has stood a while.
    pass(&t, VIS * 7).await;
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
}

#[tokio::test]
async fn no_route_writes_an_oversized_string() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let big = "x".repeat(5000);
    let imported = |source: &str, upstream_id: &str, title: &str| WorkRecord::Imported {
        item: item("big"),
        source: source.into(),
        upstream_id: upstream_id.into(),
        title: title.into(),
    };
    // Ingest.
    for r in [
        imported(&big, "o/r#1", "t"),
        imported("github", &big, "t"),
        imported("github", "o/r#1", &big),
    ] {
        let err = t.ingest_record(key("ingest-gh"), r).await.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
    }
    t.ingest_record(key("ingest-gh"), imported("github", "o/r#1", "t"))
        .await
        .unwrap();
    let err = t
        .ingest_record(
            key("ingest-gh"),
            WorkRecord::Updated {
                item: item("big"),
                title: Some(big.clone()),
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
    // A person.
    for r in [
        WorkRecord::Triaged {
            item: item("big"),
            tier: 1,
            why: big.clone(),
            duplicate_of: None,
            unclear: None,
            depends_on: Vec::new(),
            split: Vec::new(),
        },
        WorkRecord::RulesSet {
            target: kbl_core::work::Target {
                source: "github".into(),
                repo: big.clone(),
            },
            root: None,
            text: None,
            checks: None,
        },
    ] {
        let err = t.person_record(key("gab"), r).await.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
    }
    let progress = |branch: Option<String>, pr: Option<String>| WorkRecord::Progress {
        item: item("big"),
        attempt: 1,
        note: kbl_core::content::Content::Text("ok".into()),
        branch,
        pr,
    };
    for r in [
        progress(Some(big.clone()), None),
        progress(None, Some(big.clone())),
    ] {
        let err = t.person_record(key("gab"), r).await.unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err}");
    }
    // An agent's release and give-up reasons.
    t.person_record(
        key("gab"),
        WorkRecord::Accepted {
            item: item("big"),
            tier: Some(2),
        },
    )
    .await
    .unwrap();
    claim(&t, "w1").await;
    let err = t
        .release(&agent("w1"), &item("big"), Some(big.clone()), 0)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
    let err = t
        .give_up(&agent("w1"), &item("big"), big, 0)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
    // The claim stands: nothing above changed it.
    t.release(&agent("w1"), &item("big"), Some("short".into()), 0)
        .await
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_republish_that_loses_a_race_with_a_claim_is_not_recorded() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    pass(&t, VIS * 7).await;
    // The sync publishes a second record and stops before it takes the lock again.
    fake.hold_next_publish();
    let sync = tokio::spawn({
        let t = t.clone();
        async move { t.sync_ready().await }
    });
    while fake.ready_count("work.ready.p2") < 2 {
        tokio::task::yield_now().await;
    }
    // Meanwhile w1 claims through the first record.
    assert!(t.try_claim(&agent("w1"), &[]).await.unwrap().is_some());
    fake.open_gate();
    sync.await.unwrap();
    // The second record is stale: w2's poll acks it and does not take w1's item.
    assert!(t.try_claim(&agent("w2"), &[]).await.unwrap().is_none());
    assert!(t.0.core.lock().await.state.held_by("w1").is_some());
    assert!(!fake.kinds().contains(&"released".to_owned()));
}

#[tokio::test]
async fn an_agent_cannot_replace_a_context_entry_it_did_not_write() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    let text = |s: &str| kbl_core::content::Content::Text(s.into());
    t.person_record(
        key("gab"),
        WorkRecord::Context {
            item: item("a1"),
            entry: key("note"),
            text: text("from a person"),
        },
    )
    .await
    .unwrap();
    let before = fake.log().len();
    let refused = |r: Result<serde_json::Value, Error>| {
        assert!(
            matches!(r, Err(Error::Refused { ref why }) if why == "not_your_entry"),
            "{r:?}"
        );
    };
    refused(
        t.context_add(&agent("w1"), &item("a1"), key("note"), text("forged"))
            .await,
    );
    // The record never reached the log.
    assert_eq!(fake.log().len(), before);
    // Its own entry can be replaced, by itself.
    t.context_add(&agent("w1"), &item("a1"), key("mine"), text("one"))
        .await
        .unwrap();
    t.context_add(&agent("w1"), &item("a1"), key("mine"), text("two"))
        .await
        .unwrap();
    // A person takes it over; the agent cannot take it back.
    t.person_record(
        key("gab"),
        WorkRecord::Context {
            item: item("a1"),
            entry: key("mine"),
            text: text("person"),
        },
    )
    .await
    .unwrap();
    refused(
        t.context_add(&agent("w1"), &item("a1"), key("mine"), text("back"))
            .await,
    );
    // A later holder cannot update a previous agent's entry.
    t.context_add(&agent("w1"), &item("a1"), key("w1-note"), text("x"))
        .await
        .unwrap();
    t.release(&agent("w1"), &item("a1"), None, 0).await.unwrap();
    claim(&t, "w2").await;
    refused(
        t.context_add(&agent("w2"), &item("a1"), key("w1-note"), text("taken"))
            .await,
    );
}

#[tokio::test(start_paused = true)]
async fn a_long_hold_does_not_make_the_record_look_old_after_a_release() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    // Longer than the lapse (six periods), so only a clock that stops while the
    // item is held keeps the release from looking overdue.
    for _ in 0..15 {
        pass(&t, VIS / 2).await;
        t.extend(&agent("w1"), &item("a1")).await.unwrap();
    }
    t.release(&agent("w1"), &item("a1"), None, 0).await.unwrap();
    assert_eq!(fake.ready_count("work.ready.p2"), 1);
}

#[tokio::test(start_paused = true)]
async fn a_record_adopted_after_a_restart_is_published_again_once_dead_lettered() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    drop(t);
    // The new tracker takes back w1's delivery and so adopts its record.
    let t = start(&fake).await;
    t.extend(&agent("w1"), &item("a1")).await.unwrap();
    t.release(&agent("w1"), &item("a1"), None, 0).await.unwrap();
    fake.dead_letter("work.ready.p2", 0);
    assert_eq!(claimed_id(&claim(&t, "w3").await), None);
    pass(&t, VIS * 7).await;
    assert_eq!(claimed_id(&claim(&t, "w3").await).as_deref(), Some("a1"));
}

/// Skipping the record would fold a state its writer did not mean.
#[tokio::test]
async fn a_tracker_does_not_serve_a_log_that_holds_a_record_of_a_newer_format() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 1).await;
    let newer = br#"{"v":999,"at_ms":5,"by":"gab","actor":"person","kind":"accepted","item":"a1","tier":0}"#;
    fake.publish(WORK_EVENTS, newer.to_vec()).await.unwrap();

    fake.expire_lease();
    let Err(e) = Tracker::start(fake.reconnect(), settings()).await else {
        panic!("a tracker started on a log it cannot read");
    };
    let why = format!("{e:#}");
    assert!(
        why.contains("offset 2") && why.contains("format 999"),
        "{why}"
    );
    // It never took the lease, so a build that reads the log can start at once.
    assert!(!fake.lease_is_held());

    // A payload that is no record of any format is still skipped, as before.
    let fake = Fake::default();
    fake.publish(WORK_EVENTS, b"not a record".to_vec())
        .await
        .unwrap();
    let t = start(&fake).await;
    seed(&t, "a1", 1).await;
    assert_eq!(fake.ready_count("work.ready.p1"), 1);
}

#[tokio::test]
async fn a_raise_past_what_the_tracker_reads_is_refused_and_not_logged() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let err = t
        .person_record(
            key("gab"),
            WorkRecord::FormatRaised {
                to: kbl_core::work::FORMAT + 1,
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Refused { why } if why == "format"));
    assert!(fake.log().is_empty());
}

/// `returned_to_scope` is format 2: refused until a person raises the log,
/// then the item is claimable again.
#[tokio::test]
async fn ingest_returns_an_item_to_scope_once_the_format_is_raised() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 1).await;
    t.ingest_record(key("ingest-gh"), WorkRecord::LeftScope { item: item("a1") })
        .await
        .unwrap();
    assert_eq!(claimed_id(&claim(&t, "w1").await), None);
    let back = || WorkRecord::ReturnedToScope { item: item("a1") };
    let err = t.ingest_record(key("ingest-gh"), back()).await.unwrap_err();
    assert!(matches!(err, Error::Refused { why } if why == "format"));

    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 2 })
        .await
        .unwrap();
    t.ingest_record(key("ingest-gh"), back()).await.unwrap();
    let log = fake.log();
    let last = log.last().unwrap();
    assert_eq!((last.v, last.record.kind()), (2, "returned_to_scope"));
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
}

#[tokio::test]
async fn a_record_of_an_unknown_kind_is_refused_and_not_logged() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let unknown: WorkRecord = serde_json::from_str(r#"{"kind":"teleported","item":"a1"}"#).unwrap();
    let err = t.person_record(key("gab"), unknown).await.unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
    assert!(fake.log().is_empty());
}
