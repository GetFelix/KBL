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
    extend_stale: bool,
}

/// An in-memory Felix: streams, and a consumer group per ready stream. A `Fake`
/// is one tracker's connection; `reconnect` is a restarted tracker. Only a
/// member's first poll on a connection reclaims what Felix still holds for it.
#[derive(Clone, Default)]
struct Fake {
    net: Arc<StdMutex<Net>>,
    polled: Arc<StdMutex<HashSet<String>>>,
}

impl Fake {
    fn reconnect(&self) -> Fake {
        Fake {
            net: self.net.clone(),
            polled: Arc::default(),
        }
    }

    fn log(&self) -> Vec<WorkLogged> {
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

    fn kinds(&self) -> Vec<String> {
        self.log()
            .iter()
            .map(|l| l.record.kind().to_owned())
            .collect()
    }

    fn calls(&self) -> Vec<String> {
        self.net.lock().unwrap().calls.clone()
    }

    /// The visibility timeout passes for everything leased.
    fn lapse_all(&self) {
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

    fn fail_publishes_to(&self, stream: Option<&str>) {
        self.net.lock().unwrap().fail_publish_on = stream.map(str::to_owned);
    }

    fn extends_are_stale(&self, stale: bool) {
        self.net.lock().unwrap().extend_stale = stale;
    }

    /// A poll by a tracker that then died before it wrote anything.
    fn poll_and_vanish(&self, stream: &str, member: &str) -> Option<GroupRecord> {
        self.poll_sync(stream, member)
    }

    fn ready_count(&self, stream: &str) -> usize {
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
        let mut net = self.net.lock().unwrap();
        if net.fail_publish_on.as_deref() == Some(stream) {
            anyhow::bail!("the broker is down");
        }
        let log = net.streams.entry(stream.to_owned()).or_default();
        let offset = log.len() as u64;
        log.push((offset, payload));
        net.calls.push(format!("publish {stream}"));
        Ok(offset)
    }

    async fn read_from(&self, stream: &str, from: u64) -> anyhow::Result<Vec<(u64, Vec<u8>)>> {
        let net = self.net.lock().unwrap();
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

    async fn ack(&self, stream: &str, _member: &str, offset: u64) -> anyhow::Result<()> {
        let mut net = self.net.lock().unwrap();
        net.slots.insert((stream.to_owned(), offset), Slot::Acked);
        net.calls.push(format!("ack {stream}"));
        Ok(())
    }

    async fn nack(&self, stream: &str, _member: &str, record: &GroupRecord) -> anyhow::Result<()> {
        let mut net = self.net.lock().unwrap();
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
        let net = self.net.lock().unwrap();
        let mine = net.slots.get(&(stream.to_owned(), record.offset))
            == Some(&Slot::Leased(member.to_owned()));
        if net.extend_stale || !mine {
            return Err(ExtendError::Stale);
        }
        Ok(by)
    }
}

fn agent(name: &str) -> AgentName {
    AgentName::new(name).unwrap()
}

fn item(id: &str) -> ItemId {
    ItemId::new(id).unwrap()
}

fn key(k: &str) -> Key {
    Key::new(k).unwrap()
}

/// A person imports an item and accepts it at `tier`.
async fn seed(t: &Tracker<Fake>, id: &str, tier: u8) {
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

async fn start(fake: &Fake) -> Tracker<Fake> {
    Tracker::start(fake.reconnect(), VIS, false).await.unwrap()
}

async fn claim(t: &Tracker<Fake>, who: &str) -> Value {
    t.next(&agent(who), Duration::ZERO).await.unwrap()
}

fn claimed_id(v: &Value) -> Option<String> {
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
            why: agents_core::work::CloseWhy::Dismissed,
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
async fn a_holder_whose_delivery_is_gone_after_a_restart_keeps_its_item_and_gets_no_second() {
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
    let again = claim(&second, "w1").await;
    assert_eq!(
        (again["resumed"].as_bool(), claimed_id(&again).as_deref()),
        (Some(true), Some("a1"))
    );
    assert_eq!(fake.kinds().iter().filter(|k| *k == "claimed").count(), 1);
    assert!(matches!(
        second.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Lost(_))
    ));
    // The holder can still finish what the log says it holds.
    second.complete(&agent("w1"), &item("a1"), 0).await.unwrap();
    assert_eq!(fake.kinds().last().map(String::as_str), Some("done"));
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
            why: agents_core::work::CloseWhy::Dismissed,
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
