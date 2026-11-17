use std::collections::{HashMap, HashSet};
use std::sync::Mutex as StdMutex;

use bytes::Bytes;

use super::*;
use kbl_core::work::PriceUnit;

const VIS: Duration = Duration::from_secs(90);

#[derive(Clone, PartialEq, Debug)]
enum Slot {
    Pending,
    Leased(String),
    Acked,
}

impl Net {
    /// The offset the stream's next record gets, in flight ones counted.
    fn next_offset(&self, stream: &str) -> u64 {
        let last = |m: &HashMap<String, Vec<(u64, Vec<u8>)>>| {
            m.get(stream)
                .and_then(|v| v.iter().map(|(o, _)| o + 1).max())
                .unwrap_or(0)
        };
        let blank = self
            .blanks
            .get(stream)
            .and_then(|b| b.iter().next_back().map(|o| o + 1))
            .unwrap_or(0);
        last(&self.streams).max(last(&self.in_flight)).max(blank)
    }

    /// The offset after the last committed record or blank, which is where
    /// Felix's empty read page points. Offsets in flight are not counted.
    fn committed_end(&self, stream: &str) -> u64 {
        let record = (self.streams.get(stream))
            .and_then(|v| v.iter().map(|(o, _)| o + 1).max())
            .unwrap_or(0);
        let blank = (self.blanks.get(stream))
            .and_then(|b| b.iter().next_back().map(|o| o + 1))
            .unwrap_or(0);
        record.max(blank)
    }
}

#[derive(Default)]
struct Net {
    streams: HashMap<String, Vec<(u64, Vec<u8>)>>,
    /// Offsets that hold no record: a Raft shard's blank entries. Readers
    /// never see one; the stream's next offset and tip pass it.
    blanks: HashMap<String, std::collections::BTreeSet<u64>>,
    /// A refused conditional publish stores a blank at the offset it would
    /// have taken, as on a Raft stream shard. Off, a refusal consumes none.
    raft_blanks: bool,
    /// Records another writer was given offsets for that are not committed
    /// yet: Felix's tail counts them, reads do not show them.
    in_flight: HashMap<String, Vec<(u64, Vec<u8>)>>,
    slots: HashMap<(String, u64), Slot>,
    /// How many times each record has been handed out.
    tries: HashMap<(String, u64), u32>,
    /// Every broker call that changes something, in order.
    calls: Vec<String>,
    fail_publish_on: Option<String>,
    /// The next conditional publish to this stream lands, and its reply is lost.
    lose_publish_reply: Option<String>,
    /// That lost write is also cut, as by a failover before a quorum held it.
    cut_lost_write: bool,
    /// The offset of the last write cut that way, until it is taken.
    was_cut: Option<u64>,
    /// Every record a conditional publish acknowledged, by stream.
    acked: HashMap<String, Vec<(u64, Vec<u8>)>>,
    /// Publishes to the stream that still succeed, then every one fails.
    publish_budget: Option<(String, u32)>,
    fail_reads: bool,
    /// Most records one `read_page` answers, like Felix's read cap. None reads all.
    page_size: Option<usize>,
    extend_stale: bool,
    /// The writer lease key: holder and version, while it is live.
    lease: Option<(Vec<u8>, u64)>,
    lease_versions: u64,
    /// The lease key runs out after its time to live, as Felix's does.
    lease_expires: bool,
    lease_until: Option<tokio::time::Instant>,
    /// How much longer than its time to live the entry stands: Felix starts it
    /// when the request lands, after the tracker started its own clock.
    lease_lag: Duration,
    /// Polls by this member never answer.
    hang_member: Option<String>,
    /// Polls of every stream that starts with this fail, as during a
    /// ready-lane failover.
    fail_polls: Option<String>,
    /// Members whose connection has dropped: their calls fail until it is forgotten.
    dead: HashSet<String>,
    forgets: Vec<Option<String>>,
    fail_lease: bool,
    gate_extend: bool,
    /// The next publish waits for `open_gate` after the record is on the stream.
    gate_publish: bool,
    /// Publishes to every stream that starts with this never answer.
    hang_publishes: Option<String>,
    /// The next lease put waits for the gate before it reaches the key.
    gate_lease: bool,
    /// The next this many lease puts land, but their replies are lost.
    lose_lease_reply: u32,
    /// `work.events` is no longer acknowledged durably.
    acks_weakened: bool,
    /// Reads of the lease key fail.
    fail_lease_get: bool,
    /// The entry runs out right after the next read of the lease key.
    expire_after_lease_get: bool,
    /// Reads of the lease key see its holder at this older version.
    lagging_lease_get: Option<u64>,
    /// The next nack waits for `open_gate` before it lands.
    gate_nack: bool,
    /// The next read waits for `open_gate` before it reads.
    gate_read: bool,
    lease_puts: u32,
    /// The next nack panics.
    panic_nack: bool,
    /// Ingest's saved state by name, with versions.
    cursors: HashMap<String, (Vec<u8>, u64)>,
}

/// How many times Felix hands a record out before it is dead-lettered.
const ATTEMPT_BOUND: u32 = 5;

/// An in-memory Felix: streams, and a consumer group per ready stream. A `Fake`
/// is one tracker's connection; `reconnect` is a restarted tracker. Only a
/// member's first poll of a lane on a connection reclaims what Felix still
/// holds for it there.
#[derive(Clone, Default)]
pub(super) struct Fake {
    net: Arc<StdMutex<Net>>,
    gate: Arc<tokio::sync::Notify>,
    polled: Arc<StdMutex<HashSet<String>>>,
}

impl Fake {
    /// Retention: drops the records before `before`, as a trimmed stream.
    pub(super) fn trim_before(&self, stream: &str, before: u64) {
        let mut net = self.net.lock().unwrap();
        if let Some(log) = net.streams.get_mut(stream) {
            log.retain(|(o, _)| *o >= before);
        }
    }

    pub(super) fn page_size(&self, size: usize) {
        self.net.lock().unwrap().page_size = Some(size);
    }

    /// Makes the stream behave like a Raft shard: a refused conditional
    /// publish burns the offset it asked for (Felix docs/per-shard-raft.md,
    /// "Conditional publishes").
    pub(super) fn raft_blanks(&self) {
        self.net.lock().unwrap().raft_blanks = true;
    }

    /// An offset that holds no record, as a new leader's term-start blank.
    pub(super) fn blank(&self, stream: &str) {
        let mut net = self.net.lock().unwrap();
        let at = net.next_offset(stream);
        net.blanks.entry(stream.to_owned()).or_default().insert(at);
    }

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
        let offset = net.next_offset(stream);
        net.streams
            .entry(stream.to_owned())
            .or_default()
            .push((offset, payload));
        net.calls.push(format!("publish {stream}"));
        Ok((offset, std::mem::take(&mut net.gate_publish)))
    }

    /// Another writer is given the next offset; the record is not committed yet.
    fn claim_in_flight(&self, stream: &str, payload: Vec<u8>) {
        let mut net = self.net.lock().unwrap();
        let offset = net.next_offset(stream);
        net.in_flight
            .entry(stream.to_owned())
            .or_default()
            .push((offset, payload));
    }

    /// The in-flight records commit, in offset order.
    fn land_in_flight(&self, stream: &str) {
        let mut net = self.net.lock().unwrap();
        let landed = net.in_flight.remove(stream).unwrap_or_default();
        let log = net.streams.entry(stream.to_owned()).or_default();
        log.extend(landed);
        log.sort_by_key(|(o, _)| *o);
    }

    fn lift_publish_budget(&self) {
        self.net.lock().unwrap().publish_budget = None;
    }

    fn fail_publishes_to_after(&self, stream: &str, ok: u32) {
        self.net.lock().unwrap().publish_budget = Some((stream.to_owned(), ok));
    }

    pub(super) fn lose_next_publish_reply(&self, stream: &str) {
        self.net.lock().unwrap().lose_publish_reply = Some(stream.to_owned());
    }

    /// The next conditional publish to `stream` lands and a failover cuts it
    /// before anyone is told or reads it: the only kind of record a durable
    /// stream may lose.
    pub(super) fn cut_next_lost_write(&self, stream: &str) {
        let mut net = self.net.lock().unwrap();
        net.lose_publish_reply = Some(stream.to_owned());
        net.cut_lost_write = true;
    }

    /// The offset a lost write was cut from, if one was since the last call.
    /// A cut not yet made is called off.
    pub(super) fn take_cut(&self) -> Option<u64> {
        let mut net = self.net.lock().unwrap();
        if std::mem::take(&mut net.cut_lost_write) {
            net.lose_publish_reply = None;
        }
        net.was_cut.take()
    }

    /// The first record a conditional publish acknowledged on `stream` that
    /// the stream no longer holds as written.
    pub(super) fn cut_acked(&self, stream: &str) -> Option<u64> {
        let net = self.net.lock().unwrap();
        let log = net
            .streams
            .get(stream)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let acked = net.acked.get(stream)?;
        acked.iter().find(|r| !log.contains(r)).map(|(o, _)| *o)
    }

    fn fail_publishes_to(&self, stream: Option<&str>) {
        self.net.lock().unwrap().fail_publish_on = stream.map(str::to_owned);
    }

    /// An agent's connection to the broker drops.
    fn drop_connection(&self, member: &str) {
        self.net.lock().unwrap().dead.insert(member.to_owned());
    }

    pub(super) fn forgets(&self) -> Vec<Option<String>> {
        self.net.lock().unwrap().forgets.clone()
    }

    fn fail_lease(&self, fail: bool) {
        self.net.lock().unwrap().fail_lease = fail;
    }

    /// The next extend waits for `open_gate`.
    fn hold_extends(&self) {
        self.net.lock().unwrap().gate_extend = true;
    }

    fn hang_publishes_to(&self, streams: Option<&str>) {
        self.net.lock().unwrap().hang_publishes = streams.map(str::to_owned);
    }

    fn hold_next_publish(&self) {
        self.net.lock().unwrap().gate_publish = true;
    }

    fn hold_next_read(&self) {
        self.net.lock().unwrap().gate_read = true;
    }

    fn lease_puts(&self) -> u32 {
        self.net.lock().unwrap().lease_puts
    }

    pub(super) fn weaken_acks(&self, weak: bool) {
        self.net.lock().unwrap().acks_weakened = weak;
    }

    fn fail_lease_gets(&self, fail: bool) {
        self.net.lock().unwrap().fail_lease_get = fail;
    }

    fn expire_lease_after_next_get(&self) {
        self.net.lock().unwrap().expire_after_lease_get = true;
    }

    fn lease_gets_lag_at(&self, version: Option<u64>) {
        self.net.lock().unwrap().lagging_lease_get = version;
    }

    fn lease_version(&self) -> Option<u64> {
        self.net.lock().unwrap().lease.as_ref().map(|(_, v)| *v)
    }

    fn lose_next_lease_reply(&self) {
        self.net.lock().unwrap().lose_lease_reply += 1;
    }

    fn hold_next_lease_put(&self) {
        self.net.lock().unwrap().gate_lease = true;
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

    pub(super) fn fail_polls(&self, streams: Option<&str>) {
        self.net.lock().unwrap().fail_polls = streams.map(str::to_owned);
    }

    /// How many times Felix has handed out each record of `stream`, by offset.
    pub(super) fn tries(&self, stream: &str) -> Vec<(u64, u32)> {
        let net = self.net.lock().unwrap();
        let mut tries: Vec<(u64, u32)> = net
            .tries
            .iter()
            .filter(|((s, _), _)| s == stream)
            .map(|((_, o), n)| (*o, *n))
            .collect();
        tries.sort();
        tries
    }

    fn hang(&self, member: Option<&str>) {
        self.net.lock().unwrap().hang_member = member.map(str::to_owned);
    }

    /// The lease key's time to live passes.
    fn lease_expires(&self) {
        self.net.lock().unwrap().lease_expires = true;
    }

    /// The lease key stands `lag` past the tracker's view of it.
    fn lease_lags(&self, lag: Duration) {
        self.net.lock().unwrap().lease_lag = lag;
    }

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
        // Like Felix: a reclaim is per lane, once per connection.
        let reclaim = self
            .polled
            .lock()
            .unwrap()
            .insert(format!("{member} {stream}"));
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
                    pick = Some(offset);
                    break;
                }
                Slot::Pending if pick.is_none() => {
                    pick = Some(offset);
                    // A reclaim looks at the whole stream before it takes new work.
                    if !reclaim {
                        break;
                    }
                }
                _ => {}
            }
        }
        let offset = pick?;
        let key = (stream.to_owned(), offset);
        // Felix counts a reclaim as an attempt too.
        let tries = net.tries.entry(key.clone()).or_insert(0);
        *tries += 1;
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
        let hangs = (self.net.lock().unwrap().hang_publishes.as_deref())
            .is_some_and(|p| stream.starts_with(p));
        if hangs {
            std::future::pending::<()>().await;
        }
        let (offset, gated) = self.publish_now(stream, payload)?;
        if gated {
            self.gate.notified().await;
        }
        Ok(offset)
    }

    async fn publish_if(
        &self,
        stream: &str,
        payload: Vec<u8>,
        expected: u64,
    ) -> anyhow::Result<ConditionalWrite> {
        // The reply is lost, whatever Felix did.
        if self.net.lock().unwrap().fail_publish_on.as_deref() == Some(stream) {
            anyhow::bail!("the broker is down");
        }
        // Like Felix: refused unless it would land at the stream's next
        // offset, which counts offsets given out but not yet committed.
        let tail = self.net.lock().unwrap().next_offset(stream);
        if expected != tail {
            let mut net = self.net.lock().unwrap();
            if net.raft_blanks {
                // The blank takes the offset the record would have; the tail
                // the refusal names is the one after it.
                net.blanks
                    .entry(stream.to_owned())
                    .or_default()
                    .insert(tail);
                return Ok(ConditionalWrite::Refused { tail: tail + 1 });
            }
            return Ok(ConditionalWrite::Refused { tail });
        }
        let offset = self.publish(stream, payload.clone()).await?;
        let mut net = self.net.lock().unwrap();
        if net.lose_publish_reply.take_if(|s| s == stream).is_some() {
            if std::mem::take(&mut net.cut_lost_write)
                && let Some(records) = net.streams.get_mut(stream)
            {
                records.retain(|(o, _)| *o != offset);
                net.was_cut = Some(offset);
            }
            anyhow::bail!("the reply was lost");
        }
        let acked = net.acked.entry(stream.to_owned()).or_default();
        acked.push((offset, payload));
        Ok(ConditionalWrite::Written { offset })
    }

    async fn read_from(&self, stream: &str, from: u64) -> anyhow::Result<Vec<(u64, Vec<u8>)>> {
        let gated = std::mem::take(&mut self.net.lock().unwrap().gate_read);
        if gated {
            self.gate.notified().await;
        }
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

    async fn read_to_tip(&self, stream: &str, from: u64) -> anyhow::Result<crate::felix::ToTip> {
        let records = self.read_from(stream, from).await?;
        // Like Felix's empty page: the tip is where the committed log ends,
        // which a trailing blank puts past the last record.
        let end = self.net.lock().unwrap().committed_end(stream);
        let tip = records.last().map_or(from, |(o, _)| o + 1).max(end);
        Ok((records, tip))
    }

    async fn read_page(&self, stream: &str, from: u64) -> anyhow::Result<crate::felix::Page> {
        let all = self.read_from(stream, from).await?;
        let Some(size) = self.net.lock().unwrap().page_size else {
            return Ok((all, None));
        };
        // Like Felix: a page names the offset after it, and an empty page is the tip.
        let page: Vec<_> = all.into_iter().take(size).collect();
        let next = page.last().map(|(o, _)| o + 1);
        Ok((page, next))
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
        if (self.net.lock().unwrap().fail_polls.as_deref()).is_some_and(|p| stream.starts_with(p)) {
            anyhow::bail!("the ready lane is failing over");
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
        ttl: Duration,
        expect: Option<u64>,
    ) -> anyhow::Result<Option<u64>> {
        let gated = std::mem::take(&mut self.net.lock().unwrap().gate_lease);
        if gated {
            self.gate.notified().await;
        }
        let mut net = self.net.lock().unwrap();
        net.lease_puts += 1;
        if net.fail_lease {
            anyhow::bail!("the broker is down");
        }
        let now = tokio::time::Instant::now();
        if net.lease_expires && net.lease_until.is_none_or(|t| t <= now) {
            net.lease = None;
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
        net.lease_until = Some(now + ttl + net.lease_lag);
        if net.lose_lease_reply > 0 {
            net.lose_lease_reply -= 1;
            anyhow::bail!("Felix did not answer in time");
        }
        Ok(Some(v))
    }

    async fn lease_get(&self) -> anyhow::Result<Option<(Vec<u8>, u64)>> {
        let mut net = self.net.lock().unwrap();
        if net.fail_lease || net.fail_lease_get {
            anyhow::bail!("the broker is down");
        }
        let now = tokio::time::Instant::now();
        if net.lease_expires && net.lease_until.is_none_or(|t| t <= now) {
            net.lease = None;
        }
        let read = match (net.lease.clone(), net.lagging_lease_get) {
            (Some((holder, _)), Some(old)) => Some((holder, old)),
            (lease, _) => lease,
        };
        if std::mem::take(&mut net.expire_after_lease_get) {
            net.lease = None;
        }
        Ok(read)
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

    async fn check_acks(&self) -> anyhow::Result<()> {
        if self.net.lock().unwrap().acks_weakened {
            anyhow::bail!("work.events is no longer durable");
        }
        Ok(())
    }

    async fn forget(&self, member: Option<&str>) {
        let mut net = self.net.lock().unwrap();
        net.forgets.push(member.map(str::to_owned));
        if let Some(m) = member {
            net.dead.remove(m);
            let prefix = format!("{m} ");
            self.polled
                .lock()
                .unwrap()
                .retain(|p| !p.starts_with(&prefix));
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
        crash_after_done: false,
        review_tiers: default_review_tiers(),
        cost_floor_micros: 0,
        team: String::new(),
        review_limit: 0,
        snapshot_dir: None,
        snapshot_every: 0,
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
    let reply = t
        .complete(&agent("w1"), &item("a1"), None, Vec::new())
        .await
        .unwrap();
    assert_eq!(reply["status"], "done");
    let calls = fake.calls();
    let done = calls
        .iter()
        .rposition(|c| c == "publish work.events")
        .unwrap();
    let ack = calls.iter().position(|c| c == "ack work.ready.p2").unwrap();
    assert!(done < ack, "{calls:?}");
    assert_eq!(fake.log().last().unwrap().record.kind(), "done");
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
        team: String::new(),
        review_limit: 0,
        ..settings()
    }
}

fn tokens(model: &str, input: u64) -> Usage {
    Usage::Tokens {
        model: model.into(),
        input,
        output: 0,
        cache_read: 0,
        cache_write: 0,
    }
}

#[tokio::test]
async fn below_format_8_a_claim_is_charged_the_floor_and_usage_is_dropped() {
    let fake = Fake::default();
    let t = start_with(&fake, floor(500)).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    t.release(&agent("w1"), &item("a1"), None, None, Vec::new())
        .await
        .unwrap();
    claim(&t, "w1").await;
    t.give_up(
        &agent("w1"),
        &item("a1"),
        None,
        "stuck".into(),
        vec![tokens("m", 9)],
    )
    .await
    .unwrap();
    assert_eq!(costs(&fake, "released"), [500]);
    assert_eq!(costs(&fake, "gave_up"), [500]);
    assert!(
        fake.log()
            .iter()
            .all(|l| l.record.charge().is_none_or(|c| c.usage.is_empty()))
    );
    seed(&t, "a4", 2).await;
    let core = t.0.core.lock().await;
    let left = |id: &str| core.state.budgets[&item(id)].cost_left_micros;
    assert_eq!(left("a1") + 1000, left("a4"));
}

/// From format 8 the record carries the usage and the floor, and the fold
/// prices it: the budget draws the priced cost, or the floor if that is more.
#[tokio::test]
async fn from_format_8_usage_is_priced_and_floored() {
    let fake = Fake::default();
    let t = start_with(&fake, floor(500)).await;
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 8 })
        .await
        .unwrap();
    t.person_record(
        key("gab"),
        WorkRecord::PriceSet {
            model: "m".into(),
            unit: PriceUnit::Input,
            price_micros: 3_000_000,
            effective_from_ms: None,
            as_of: None,
        },
    )
    .await
    .unwrap();
    for id in ["a1", "a2", "a3"] {
        seed(&t, id, 2).await;
    }
    claim(&t, "w1").await;
    t.complete(
        &agent("w1"),
        &item("a1"),
        None,
        vec![tokens("m", 1_000_000)],
    )
    .await
    .unwrap();
    claim(&t, "w1").await;
    t.complete(&agent("w1"), &item("a2"), None, vec![tokens("m", 10)])
        .await
        .unwrap();
    claim(&t, "w1").await;
    t.complete(&agent("w1"), &item("a3"), None, vec![tokens("unknown", 10)])
        .await
        .unwrap();
    let log = fake.log();
    let done: Vec<_> = log.iter().filter(|l| l.record.kind() == "done").collect();
    let c = done[0].record.charge().unwrap();
    assert_eq!((c.cost_micros, c.floor_micros, c.usage.len()), (0, 500, 1));
    let core = t.0.core.lock().await;
    let cost = |id: &str| core.state.items[&item(id)].cost_micros;
    assert_eq!(cost("a1"), 3_000_000);
    assert_eq!(cost("a2"), 500);
    // An unknown model is recorded, not refused, and costs the floor until priced.
    assert_eq!(cost("a3"), 500);
    assert_eq!(core.state.spend().unpriced.len(), 1);
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
        t.complete(&agent("w1"), &item("a1"), None, Vec::new())
            .await,
        Err(Error::Lost(_))
    ));
}

#[tokio::test]
async fn an_item_the_agent_released_is_handed_out_again() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    t.release(
        &agent("w1"),
        &item("a1"),
        None,
        Some("stuck".into()),
        Vec::new(),
    )
    .await
    .unwrap();
    assert!(fake.calls().contains(&"nack work.ready.p2".to_owned()));
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a1"));
    // The second release uses the item's second try; the item then waits for a person.
    t.release(&agent("w2"), &item("a1"), None, None, Vec::new())
        .await
        .unwrap();
    assert_eq!(claimed_id(&claim(&t, "w1").await), None);
    assert!(fake.calls().contains(&"ack work.ready.p2".to_owned()));
}

/// Another writer logs w1's release behind the tracker's back, as a failover
/// can: the tracker's memory still shows w1 holding a1.
async fn release_behind_the_trackers_back(fake: &Fake) {
    let by = fake
        .log()
        .into_iter()
        .find(|l| matches!(l.record, WorkRecord::Claimed { .. }))
        .expect("a claim")
        .by;
    let released = WorkRecord::Released {
        item: item("a1"),
        attempt: 1,
        reason: None,
        cost_micros: 0,
        usage: Vec::new(),
        floor_micros: 0,
    };
    let logged = WorkLogged::new(0, by, Actor::Agent, released);
    fake.publish(WORK_EVENTS, logged.encode()).await.unwrap();
}

#[tokio::test]
async fn a_claim_released_behind_the_trackers_back_is_not_resumed() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    let first = claim(&t, "w1").await;
    release_behind_the_trackers_back(&fake).await;
    let again = claim(&t, "w1").await;
    // The ended claim is never handed back; the item may go out under a new one.
    assert_ne!(again["resumed"], true);
    if claimed_id(&again).is_some() {
        assert_ne!(again["offset"], first["offset"]);
    }
}

#[tokio::test]
async fn a_claim_released_behind_the_trackers_back_is_not_extended() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    release_behind_the_trackers_back(&fake).await;
    assert!(t.extend(&agent("w1"), &item("a1")).await.is_err());
    // The retry rebuilds from the log and finds the claim gone.
    assert!(matches!(
        t.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Lost(_))
    ));
}

#[tokio::test]
async fn a_claim_a_failover_cut_from_the_log_is_not_extended() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    let first = claim(&t, "w1").await;
    let at = first["offset"].as_u64().unwrap();
    // The new leader never had the claim: the log ends before it.
    (fake
        .net
        .lock()
        .unwrap()
        .streams
        .get_mut(WORK_EVENTS)
        .unwrap())
    .retain(|(o, _)| *o < at);
    assert!(t.extend(&agent("w1"), &item("a1")).await.is_err());
    assert!(matches!(
        t.extend(&agent("w1"), &item("a1")).await,
        Err(Error::Lost(_))
    ));
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
    t.complete(&agent("w1"), &item("a1"), None, Vec::new())
        .await
        .unwrap();
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
    second
        .complete(&agent("w1"), &item("a1"), None, Vec::new())
        .await
        .unwrap();
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
            usage: Vec::new(),
            floor_micros: 0,
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
        t.complete(&agent("w1"), &item("a1"), None, Vec::new())
            .await,
        Err(Error::Lost(_))
    ));
    assert!(matches!(
        t.release(&agent("w1"), &item("a1"), None, None, Vec::new())
            .await,
        Err(Error::Lost(_))
    ));
    assert_eq!(fake.slot("work.ready.p2", 0), Slot::Leased("w2".into()));
    let acks_after = fake.calls().iter().filter(|c| c.starts_with("ack")).count();
    assert_eq!(acks_before, acks_after);
    // And w2 can still finish.
    t.complete(&agent("w2"), &item("a1"), None, Vec::new())
        .await
        .unwrap();
    assert_eq!(fake.slot("work.ready.p2", 0), Slot::Acked);
}

#[tokio::test]
async fn a_failed_reload_does_not_empty_the_state_and_nothing_is_served_until_it_works() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    fake.fail_publishes_to(Some(WORK_EVENTS));
    assert!(
        t.complete(&agent("w1"), &item("a1"), None, Vec::new())
            .await
            .is_err()
    );
    fake.fail_publishes_to(None);
    fake.fail_reads(true);
    // The state must be rebuilt, the read fails: refuse rather than serve an empty board.
    assert!(matches!(
        t.next(&agent("w2"), Duration::ZERO).await,
        Err(Error::Unavailable(_))
    ));
    assert!(matches!(
        t.complete(&agent("w1"), &item("a1"), None, Vec::new())
            .await,
        Err(Error::Unavailable(_))
    ));
    fake.fail_reads(false);
    // Rebuilt from the log: the item is still w1's, and finishes once.
    t.complete(&agent("w1"), &item("a1"), None, Vec::new())
        .await
        .unwrap();
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
        t.complete(&agent("w3"), &item("a1"), None, Vec::new())
            .await
            .unwrap();
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
    t.complete(&agent("w1"), &item("a1"), None, Vec::new())
        .await
        .unwrap();
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
        first
            .complete(&agent("w1"), &item("a1"), None, Vec::new())
            .await,
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
        t.complete(&agent("w1"), &item("a1"), None, Vec::new())
            .await,
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
    assert!(
        t.complete(&agent("w1"), &item("a1"), None, Vec::new())
            .await
            .is_err()
    );
    fake.fail_publishes_to(None);
    // The next call reloads, so the item is still held and can be finished.
    assert!(
        t.complete(&agent("w1"), &item("a1"), None, Vec::new())
            .await
            .is_ok()
    );
    assert_eq!(fake.kinds().iter().filter(|k| *k == "done").count(), 1);
}

#[tokio::test]
async fn a_record_the_fold_refuses_is_not_appended() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    let before = fake.log().len();
    // An agent that holds nothing cannot finish the item.
    let err = t
        .complete(&agent("w1"), &item("a1"), None, Vec::new())
        .await
        .unwrap_err();
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
async fn ingest_writing_a_context_entry_again_unchanged_logs_nothing() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let ingest = |r| {
        let t = &t;
        async move { t.ingest_record(key("ingest-gh"), r).await }
    };
    let imported = WorkRecord::Imported {
        item: item("r1"),
        source: "github".into(),
        upstream_id: "GetFelix/felix#1".into(),
        title: "a bug".into(),
    };
    ingest(imported).await.unwrap();
    let context = |body: &str| WorkRecord::Context {
        item: item("r1"),
        entry: key("issue"),
        text: Content::Text(body.into()),
    };
    let first = ingest(context("the body")).await.unwrap();
    let before = fake.log().len();
    let again = ingest(context("the body")).await.unwrap();
    assert_eq!(fake.log().len(), before);
    assert_eq!(again["offset"], first["offset"]);
    // The texts come back from the log on a restart.
    let t = start(&fake).await;
    t.ingest_record(key("ingest-gh"), context("the body"))
        .await
        .unwrap();
    assert_eq!(fake.log().len(), before);
    t.ingest_record(key("ingest-gh"), context("an edit"))
        .await
        .unwrap();
    assert_eq!(fake.log().len(), before + 1);
}

#[tokio::test]
async fn ingest_takes_back_an_entry_an_agent_wrote_even_with_the_same_text() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    let same = Content::Text("the body".into());
    t.context_add(&agent("w1"), &item("a1"), key("issue"), same.clone())
        .await
        .unwrap();
    let context = WorkRecord::Context {
        item: item("a1"),
        entry: key("issue"),
        text: same,
    };
    let before = fake.log().len();
    t.ingest_record(key("ingest-gh"), context.clone())
        .await
        .unwrap();
    assert_eq!(fake.log().len(), before + 1);
    // Ingest's own now, so the same text again is not logged.
    t.ingest_record(key("ingest-gh"), context).await.unwrap();
    assert_eq!(fake.log().len(), before + 1);
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
async fn ingest_unlinks_only_an_edge_its_own_link_made() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let ingest = |t: &Tracker<Fake>, r| {
        let t = t.clone();
        async move { t.ingest_record(key("ingest-gh"), r).await }
    };
    for (id, n) in [("r1", 1), ("r2", 2)] {
        ingest(
            &t,
            WorkRecord::Imported {
                item: item(id),
                source: "github".into(),
                upstream_id: format!("o/r#{n}"),
                title: "a bug".into(),
            },
        )
        .await
        .unwrap();
    }
    let linked = || WorkRecord::Linked {
        item: item("r2"),
        to: item("r1"),
    };
    let unlinked = || WorkRecord::Unlinked {
        item: item("r2"),
        to: item("r1"),
    };
    let why = |r: Result<Value, Error>| match r {
        Err(Error::Refused { why }) => why,
        other => panic!("{other:?}"),
    };
    ingest(&t, linked()).await.unwrap();
    ingest(&t, unlinked()).await.unwrap();
    assert_eq!(why(ingest(&t, unlinked()).await), "not_linked");
    // A person links the same pair: the edge is theirs too, also after a restart.
    ingest(&t, linked()).await.unwrap();
    t.person_record(key("gab"), linked()).await.unwrap();
    assert_eq!(why(ingest(&t, unlinked()).await), "not_ingests");
    let t = start(&fake).await;
    assert_eq!(why(ingest(&t, unlinked()).await), "not_ingests");
    // Ingest's link of an edge it did not make changes nothing, and is not logged.
    let before = fake.log().len();
    assert_eq!(why(ingest(&t, linked()).await), "already_linked");
    assert_eq!(fake.log().len(), before);
    // Once the person unlinks it, an edge ingest makes again is ingest's.
    t.person_record(key("gab"), unlinked()).await.unwrap();
    ingest(&t, linked()).await.unwrap();
    // A retry of its own link is accepted, and the edge stays ingest's.
    ingest(&t, linked()).await.unwrap();
    let t = start(&fake).await;
    ingest(&t, unlinked()).await.unwrap();
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
        WorkRecord::Shipped {
            item: item("theirs"),
            release: "v1".into(),
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

/// a1, imported and fixed by w1, whose request `pr` reports.
async fn fixed(t: &Tracker<Fake>, pr: Option<&str>) {
    seed_in_felix(t, "a1").await;
    claim(t, "w1").await;
    t.progress(
        &agent("w1"),
        &item("a1"),
        kbl_core::content::Content::Text("fixed".into()),
        None,
        pr.map(str::to_owned),
    )
    .await
    .unwrap();
    t.complete(&agent("w1"), &item("a1"), None, Vec::new())
        .await
        .unwrap();
}

fn shipped(tag: &str) -> WorkRecord {
    WorkRecord::Shipped {
        item: item("a1"),
        release: tag.into(),
    }
}

#[tokio::test]
async fn a_person_works_a_pull_request_and_marks_the_summary() {
    use kbl_core::work::{PrChecks, PrPhase, PrReview, PrSnapshot};
    let fake = Fake::default();
    let t = start(&fake).await;
    let pr = "GetFelix/felix#9";
    fixed(&t, Some(pr)).await;
    let person = |r| t.person_record(key("gab"), r);
    let refused = |e: Error| match e {
        Error::Refused { why } => why,
        e => panic!("{e}"),
    };
    let ask = WorkRecord::ReviewRequested {
        item: item("a1"),
        reviewer: "kim".into(),
    };
    // Before the raise, and before upstream reports the request, nothing is written.
    assert_eq!(refused(person(ask.clone()).await.unwrap_err()), "format");
    person(WorkRecord::FormatRaised { to: 11 }).await.unwrap();
    assert_eq!(refused(person(ask.clone()).await.unwrap_err()), "no_change");
    let open = |review| WorkRecord::PrStatus {
        item: item("a1"),
        status: PrSnapshot {
            pr: pr.into(),
            phase: PrPhase::Open,
            review,
            checks: PrChecks::Green,
            failing: None,
        },
    };
    t.ingest_record(key("ingest-gh"), open(PrReview::Unreviewed))
        .await
        .unwrap();
    person(ask).await.unwrap();
    person(WorkRecord::ChangesRequested { item: item("a1") })
        .await
        .unwrap();
    person(WorkRecord::SummaryRated {
        item: item("a1"),
        attempt: 1,
        accurate: false,
    })
    .await
    .unwrap();
    {
        let core = t.0.core.lock().await;
        let it = &core.state.items[&item("a1")];
        assert!(it.review_ask.is_none() && it.changes_ask.is_some());
        assert_eq!(it.paths[0].summary_accurate, Some(false));
    }
    // Open upstream, the merge is not recorded; once upstream shows it merged it is.
    assert_eq!(
        refused(
            person(WorkRecord::MergeMarked { item: item("a1") })
                .await
                .unwrap_err()
        ),
        "no_change"
    );
    t.ingest_record(
        key("ingest-gh"),
        WorkRecord::PrStatus {
            item: item("a1"),
            status: PrSnapshot {
                pr: pr.into(),
                phase: PrPhase::Merged,
                review: PrReview::Approved,
                checks: PrChecks::Green,
                failing: None,
            },
        },
    )
    .await
    .unwrap();
    person(WorkRecord::MergeMarked { item: item("a1") })
        .await
        .unwrap();
    let core = t.0.core.lock().await;
    assert!(core.state.items[&item("a1")].closed.is_some());
}

#[tokio::test]
async fn ingest_records_the_release_of_a_merged_root_once() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let pr = "GetFelix/felix#9";
    fixed(&t, Some(pr)).await;
    let merged = WorkRecord::PrStatus {
        item: item("a1"),
        status: kbl_core::work::PrSnapshot {
            pr: pr.into(),
            phase: kbl_core::work::PrPhase::Merged,
            review: kbl_core::work::PrReview::Unreviewed,
            checks: kbl_core::work::PrChecks::Green,
            failing: None,
        },
    };
    t.ingest_record(key("ingest-gh"), merged).await.unwrap();
    let close = WorkRecord::Closed {
        item: item("a1"),
        why: kbl_core::work::CloseWhy::Merged,
    };
    t.ingest_record(key("ingest-gh"), close).await.unwrap();
    t.ingest_record(key("ingest-gh"), shipped("v1"))
        .await
        .unwrap();
    let release = t.0.core.lock().await.state.items[&item("a1")]
        .release
        .clone();
    assert_eq!(release.as_deref(), Some("v1"));
    let err = t
        .ingest_record(key("ingest-gh"), shipped("v2"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Refused { ref why } if why == "already_shipped"),
        "{err}"
    );
}

#[tokio::test]
async fn ingest_cannot_ship_a_done_root_without_a_merged_request() {
    let fake = Fake::default();
    let t = start(&fake).await;
    fixed(&t, None).await;
    t.person_record(
        key("gab"),
        WorkRecord::Closed {
            item: item("a1"),
            why: kbl_core::work::CloseWhy::Completed,
        },
    )
    .await
    .unwrap();
    let before = fake.log().len();
    let err = t
        .ingest_record(key("ingest-gh"), shipped("v1"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Refused { ref why } if why == "not_merged"),
        "{err}"
    );
    assert_eq!(fake.log().len(), before);
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

mod metrics;
mod races;
mod recover;
mod renewal;
mod replay;
mod request_keys;
mod retry;
mod review;
mod review_limit;
mod sweep;
mod texts;
mod traces;

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
        .release(
            &agent("w1"),
            &item("big"),
            None,
            Some(big.clone()),
            Vec::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
    let err = t
        .give_up(&agent("w1"), &item("big"), None, big, Vec::new())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err}");
    // The claim stands: nothing above changed it.
    t.release(
        &agent("w1"),
        &item("big"),
        None,
        Some("short".into()),
        Vec::new(),
    )
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
    t.release(&agent("w1"), &item("a1"), None, None, Vec::new())
        .await
        .unwrap();
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
    t.release(&agent("w1"), &item("a1"), None, None, Vec::new())
        .await
        .unwrap();
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
    t.release(&agent("w1"), &item("a1"), None, None, Vec::new())
        .await
        .unwrap();
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

/// From format 6 an agent cannot write the `prerequisite` entry, even where
/// none exists; before it, the write folds as it always did.
#[tokio::test]
async fn an_agent_cannot_write_the_prerequisite_entry_from_format_6() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    claim(&t, "w1").await;
    let text = kbl_core::content::Content::Text("x".into());
    t.context_add(&agent("w1"), &item("a1"), key("prerequisite"), text.clone())
        .await
        .unwrap();
    t.person_record(key("gab"), WorkRecord::FormatRaised { to: 6 })
        .await
        .unwrap();
    let before = fake.log().len();
    let r = t
        .context_add(&agent("w1"), &item("a1"), key("prerequisite"), text.clone())
        .await;
    assert!(
        matches!(r, Err(Error::Refused { ref why }) if why == "not_your_entry"),
        "{r:?}"
    );
    assert_eq!(fake.log().len(), before);
    t.context_add(&agent("w1"), &item("a1"), key("other"), text)
        .await
        .unwrap();
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

#[tokio::test]
async fn a_pull_request_is_one_request_in_any_letter_case() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let report = |who: &'static str, id: &'static str, pr: &'static str| {
        let t = t.clone();
        async move {
            t.progress(
                &agent(who),
                &item(id),
                kbl_core::content::Content::Text("pr".into()),
                None,
                Some(pr.into()),
            )
            .await
        }
    };
    seed(&t, "a1", 1).await;
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
    report("w1", "a1", "GetFelix/felix#12").await.unwrap();
    seed(&t, "a2", 1).await;
    assert_eq!(claimed_id(&claim(&t, "w2").await).as_deref(), Some("a2"));
    let err = report("w2", "a2", "getfelix/FELIX#12").await.unwrap_err();
    assert!(
        matches!(&err, Error::Refused { why } if why == "pr_in_use"),
        "{err}"
    );
    // The holder's own request in another case keeps the casing on record.
    report("w1", "a1", "getfelix/felix#12").await.unwrap();
    let core = t.0.core.lock().await;
    assert_eq!(
        core.state.items[&item("a1")].pr.as_deref(),
        Some("GetFelix/felix#12")
    );
    assert_eq!(core.state.items[&item("a2")].pr, None);
}

#[tokio::test]
async fn revoking_an_agent_releases_only_its_holds_and_stops_its_claims() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    seed(&t, "a2", 2).await;
    let w1 = claimed_id(&claim(&t, "w1").await).unwrap();
    let w2 = claimed_id(&claim(&t, "w2").await).unwrap();
    assert_eq!(t.revoke(&agent("w1")).await.unwrap(), [item(&w1)]);
    let log = fake.log();
    let freed = log.last().unwrap();
    assert_eq!(
        (freed.actor, freed.by.as_str()),
        (Actor::Tracker, "tracker")
    );
    assert!(matches!(
        &freed.record,
        WorkRecord::Released { item: i, reason: Some(r), .. } if i.as_str() == w1 && r == "revoked"
    ));
    assert_eq!(t.holds_of(&agent("w2")).await, [item(&w2)]);
    assert!(matches!(
        t.extend(&agent("w1"), &item(&w1)).await,
        Err(Error::Lost(_))
    ));
    // A work_next that got past the token check before the revoke claims nothing.
    fake.lapse_all();
    let after = t.next(&agent("w1"), Duration::ZERO).await;
    assert!(
        matches!(&after, Err(Error::Refused { why }) if why == "agent_revoked"),
        "{after:?}"
    );
    assert_eq!(t.holds_of(&agent("w1")).await, Vec::<ItemId>::new());
    // The freed item goes to another agent.
    assert_eq!(
        claimed_id(&claim(&t, "w3").await).as_deref(),
        Some(w1.as_str())
    );
}

#[tokio::test(start_paused = true)]
async fn the_team_lease_is_renewed_as_soon_as_serving_starts() {
    let ttl = Duration::from_secs(3);
    // Starting the later teams took most of this team's lease, or all of it.
    for took in [Duration::from_secs(2), Duration::from_secs(4)] {
        let fake = Fake::default();
        fake.lease_expires();
        let t = start_with(
            &fake,
            Settings {
                lease_ttl: ttl,
                ..settings()
            },
        )
        .await;
        tokio::time::advance(took).await;
        let maintain = tokio::spawn({
            let t = t.clone();
            async move { t.maintain().await }
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            unavailable(&t.lease(), t.lease_margin()).is_none(),
            "not writable right after serving began, {took:?} in"
        );
        seed(&t, "a1", 2).await;
        maintain.abort();
    }
}

#[tokio::test(start_paused = true)]
async fn a_lapsed_team_lease_is_not_taken_from_a_live_holder() {
    let ttl = Duration::from_secs(3);
    let fake = Fake::default();
    fake.lease_expires();
    let t = start_with(
        &fake,
        Settings {
            lease_ttl: ttl,
            ..settings()
        },
    )
    .await;
    tokio::time::advance(Duration::from_secs(4)).await;
    // Another tracker took the lease once this one's ran out.
    let other = Tracker::start(
        fake.reconnect(),
        Settings {
            lease_ttl: ttl,
            ..settings()
        },
    )
    .await
    .unwrap();
    t.renew_lease().await;
    assert!(t.lease().fenced);
    assert!(unavailable(&other.lease(), other.lease_margin()).is_none());
}

#[tokio::test(start_paused = true)]
async fn a_lease_our_view_says_lapsed_is_renewed_while_felix_still_has_it() {
    let ttl = Duration::from_secs(3);
    let fake = Fake::default();
    fake.lease_expires();
    fake.lease_lags(Duration::from_millis(200));
    let t = start_with(
        &fake,
        Settings {
            lease_ttl: ttl,
            ..settings()
        },
    )
    .await;
    // Past the tracker's view of the lease, inside Felix's.
    tokio::time::advance(ttl + Duration::from_millis(100)).await;
    t.renew_lease().await;
    let lease = t.lease();
    assert!(!lease.fenced);
    assert!(unavailable(&lease, t.lease_margin()).is_none());
}

#[tokio::test(start_paused = true)]
async fn a_retaken_team_lease_rebuilds_from_what_another_tracker_wrote() {
    let ttl = Duration::from_secs(3);
    let short = || Settings {
        lease_ttl: ttl,
        ..settings()
    };
    let fake = Fake::default();
    fake.lease_expires();
    let first = start_with(&fake, short()).await;
    tokio::time::advance(Duration::from_secs(4)).await;
    // Another tracker held the lease after the first's ran out, wrote, and stopped.
    let second = Tracker::start(fake.reconnect(), short()).await.unwrap();
    seed(&second, "a1", 2).await;
    tokio::time::advance(Duration::from_secs(4)).await;
    first.renew_lease().await;
    assert_eq!(
        claimed_id(&claim(&first, "w1").await).as_deref(),
        Some("a1")
    );
}

#[tokio::test]
async fn an_append_on_a_state_that_may_miss_records_is_refused() {
    let fake = Fake::default();
    let t = start(&fake).await;
    let before = fake.log().len();
    // A renewal retook the lease after the call's rebuild.
    {
        let mut lease = t.lease();
        lease.stale = true;
        lease.retaken = true;
    }
    let mut core = t.0.core.lock().await;
    let record = WorkRecord::Imported {
        item: item("b1"),
        source: "gh".into(),
        upstream_id: "b1".into(),
        title: "t".into(),
    };
    assert!(matches!(
        t.append(&mut core, Actor::Person, key("gab"), record).await,
        Err(Error::Unavailable(_))
    ));
    drop(core);
    assert_eq!(fake.log().len(), before);
}

#[tokio::test]
async fn two_renewals_at_once_do_not_fence_the_holder() {
    let fake = Fake::default();
    let t = start(&fake).await;
    // The timer's renewal hangs, and a route renews meanwhile.
    fake.hold_next_lease_put();
    let slow = tokio::spawn({
        let t = t.clone();
        async move { t.renew_lease().await }
    });
    tokio::task::yield_now().await;
    let fast = tokio::spawn({
        let t = t.clone();
        async move { t.renew_lease().await }
    });
    tokio::task::yield_now().await;
    fake.open_gate();
    slow.await.unwrap();
    fast.await.unwrap();
    assert!(!t.lease().fenced);
}

#[tokio::test(start_paused = true)]
async fn a_lease_that_runs_out_while_the_renewal_is_in_flight_is_taken_again() {
    let ttl = Duration::from_secs(3);
    let fake = Fake::default();
    fake.lease_expires();
    let t = start_with(
        &fake,
        Settings {
            lease_ttl: ttl,
            ..settings()
        },
    )
    .await;
    // Our view has a little left when the renewal starts, none when it lands.
    tokio::time::advance(ttl - Duration::from_millis(50)).await;
    fake.hold_next_lease_put();
    let renew = tokio::spawn({
        let t = t.clone();
        async move { t.renew_lease().await }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    fake.open_gate();
    renew.await.unwrap();
    let lease = t.lease();
    assert!(!lease.fenced);
    assert!(lease.retaken);
    assert!(fake.lease_is_held());
}

#[tokio::test]
async fn calls_queued_behind_a_renewal_do_not_renew_again() {
    let fake = Fake::default();
    let t = start(&fake).await;
    // The lease is near its end, so every call renews before serving.
    t.lease().held.as_mut().unwrap().valid_until = Instant::now();
    let before = fake.lease_puts();
    fake.hold_next_lease_put();
    let calls: Vec<_> = (0..4)
        .map(|_| {
            let t = t.clone();
            tokio::spawn(async move { t.begin().await })
        })
        .collect();
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    fake.open_gate();
    for c in calls {
        c.await.unwrap().unwrap();
    }
    assert_eq!(fake.lease_puts() - before, 1);
}

#[tokio::test]
async fn a_rebuild_that_read_before_a_retake_stays_stale() {
    let fake = Fake::default();
    let t = start(&fake).await;
    t.lease().stale = true;
    fake.hold_next_read();
    let rebuild = tokio::spawn({
        let t = t.clone();
        async move { t.begin().await }
    });
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    {
        // A renewal retakes the lease while the rebuild reads.
        let mut lease = t.lease();
        lease.retaken = true;
        lease.retakes += 1;
    }
    fake.open_gate();
    rebuild.await.unwrap().unwrap();
    let lease = t.lease();
    assert!(lease.stale);
    assert!(lease.retaken);
}

pub(super) fn imported(id: &str) -> WorkRecord {
    WorkRecord::Imported {
        item: item(id),
        source: "gh".into(),
        upstream_id: id.into(),
        title: "t".into(),
    }
}

fn imports_of(fake: &Fake, id: &str) -> usize {
    fake.log()
        .iter()
        .filter(|l| matches!(&l.record, WorkRecord::Imported { item: i, .. } if i == &item(id)))
        .count()
}

/// While `t` was paused, another tracker took the lease.
async fn another_takes_the_lease(fake: &Fake) {
    fake.expire_lease();
    let theirs = fake
        .lease_put(b"other".to_vec(), Duration::from_secs(15), None)
        .await
        .unwrap();
    assert!(theirs.is_some());
}

#[tokio::test]
async fn a_write_felix_refused_moves_on_once_the_log_is_read_again() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    // A record this tracker did not write, with the lease still its own.
    let theirs = WorkLogged::new(0, key("gab"), Actor::Person, imported("b1"));
    fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
    assert!(matches!(
        t.person_record(key("gab"), imported("b1")).await,
        Err(Error::Unavailable(_))
    ));
    // The next call checks the lease, rebuilds, and sees b1.
    let _ = t.person_record(key("gab"), imported("b1")).await;
    assert_eq!(imports_of(&fake, "b1"), 1, "b1 was imported twice");
    t.person_record(key("gab"), imported("c1")).await.unwrap();
    assert_eq!(imports_of(&fake, "c1"), 1);
}

#[tokio::test]
async fn a_stale_team_writer_stays_out_after_felix_refuses_it() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    another_takes_the_lease(&fake).await;
    let theirs = WorkLogged::new(0, key("gab"), Actor::Person, imported("b1"));
    fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
    // Our view of the lease still stands, so only Felix can stop these.
    for _ in 0..3 {
        assert!(matches!(
            t.person_record(key("gab"), imported("c1")).await,
            Err(Error::Unavailable(_))
        ));
    }
    assert_eq!(
        imports_of(&fake, "c1"),
        0,
        "a tracker without the lease appended after the new holder's record"
    );
}

#[tokio::test(start_paused = true)]
async fn a_refusal_tail_does_not_skip_another_writers_record_in_flight() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    // Another tracker took the lease and was given the next offset for b1,
    // then lost the lease before b1 committed.
    another_takes_the_lease(&fake).await;
    let theirs = WorkLogged::new(0, key("gab"), Actor::Person, imported("b1"));
    fake.claim_in_flight(WORK_EVENTS, theirs.encode());
    fake.expire_lease();
    assert!(matches!(
        t.person_record(key("gab"), imported("c1")).await,
        Err(Error::Unavailable(_))
    ));
    // Past our view of the lease: the next call takes it again and rebuilds.
    tokio::time::advance(Duration::from_secs(16)).await;
    assert!(matches!(
        t.person_record(key("gab"), imported("c1")).await,
        Err(Error::Unavailable(_))
    ));
    assert_eq!(imports_of(&fake, "c1"), 0, "c1 skipped b1's offset");
    fake.land_in_flight(WORK_EVENTS);
    t.person_record(key("gab"), imported("c1")).await.unwrap();
    let order: Vec<String> = fake
        .log()
        .iter()
        .filter_map(|l| match &l.record {
            WorkRecord::Imported { item, .. } => Some(item.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(order, ["a1", "b1", "c1"]);
}

#[tokio::test(start_paused = true)]
async fn the_sweep_stops_after_felix_refuses_one_of_its_writes() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    seed(&t, "a2", 2).await;
    claim(&t, "w1").await;
    claim(&t, "w2").await;
    pass(&t, VIS + Duration::from_secs(1)).await;
    // A record lands that this tracker has not read.
    let theirs = WorkLogged::new(0, key("gab"), Actor::Person, imported("b1"));
    fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
    t.sweep().await;
    let released = fake
        .log()
        .iter()
        .filter(|l| l.record.kind() == "released")
        .count();
    assert_eq!(
        released, 0,
        "the sweep appended on a state its refused write had changed"
    );
}

#[tokio::test]
async fn a_lost_reply_to_a_write_also_checks_the_lease() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    another_takes_the_lease(&fake).await;
    let theirs = WorkLogged::new(0, key("gab"), Actor::Person, imported("b1"));
    fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
    // Felix refused it, but the answer never came back.
    fake.fail_publishes_to(Some(WORK_EVENTS));
    assert!(t.person_record(key("gab"), imported("c1")).await.is_err());
    fake.fail_publishes_to(None);
    assert!(matches!(
        t.person_record(key("gab"), imported("c1")).await,
        Err(Error::Unavailable(_))
    ));
    assert_eq!(imports_of(&fake, "c1"), 0);
}

#[tokio::test(start_paused = true)]
async fn after_a_refusal_the_lease_is_checked_after_the_rebuild_reads_the_log() {
    // The rebuild takes most of the 15 s lease, or this tracker's clock stood
    // still while it ran (a paused process, a sleeping laptop).
    for pause in [Duration::from_secs(9), Duration::ZERO] {
        let fake = Fake::default();
        let t = start(&fake).await;
        seed(&t, "a1", 2).await;
        // A record this tracker did not write, with the lease still its own.
        let theirs = WorkLogged::new(0, key("gab"), Actor::Person, imported("b1"));
        fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
        assert!(t.person_record(key("gab"), imported("c1")).await.is_err());
        fake.hold_next_read();
        let writing = tokio::spawn({
            let t = t.clone();
            async move { t.person_record(key("gab"), imported("c1")).await }
        });
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        // While the rebuild waits on Felix, another tracker takes the lease
        // and writes; the rebuild then reads that record.
        tokio::time::advance(pause).await;
        another_takes_the_lease(&fake).await;
        let theirs = WorkLogged::new(0, key("gab"), Actor::Person, imported("b2"));
        fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
        fake.open_gate();
        let r = writing.await.unwrap();
        assert!(matches!(r, Err(Error::Unavailable(_))), "{pause:?}: {r:?}");
        assert_eq!(
            imports_of(&fake, "c1"),
            0,
            "{pause:?}: appended after the new holder's record"
        );
    }
}

#[tokio::test]
async fn a_caller_past_begin_does_not_write_before_the_lease_is_confirmed() {
    let fake = Fake::default();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    // A record this tracker did not write, with the lease still its own.
    let theirs = WorkLogged::new(0, key("gab"), Actor::Person, imported("b1"));
    fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
    assert!(t.person_record(key("gab"), imported("c1")).await.is_err());
    // One call rebuilds and waits on its lease check. Our clock stands still.
    fake.hold_next_lease_put();
    let checking = tokio::spawn({
        let t = t.clone();
        async move { t.person_record(key("gab"), imported("c1")).await }
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(!t.lease().stale, "the rebuild has not run");
    another_takes_the_lease(&fake).await;
    // Another call already past begin(), such as the sweep, appends now.
    let mut core = t.0.core.lock().await;
    let r = t
        .append(&mut core, Actor::Person, key("gab"), imported("e1"))
        .await;
    drop(core);
    assert!(matches!(r, Err(Error::Unavailable(_))), "{r:?}");
    fake.open_gate();
    assert!(checking.await.unwrap().is_err());
    assert_eq!(
        imports_of(&fake, "e1"),
        0,
        "appended while another tracker held the lease"
    );
}

#[tokio::test(start_paused = true)]
async fn a_retaken_lease_is_confirmed_after_the_rebuild_reads_the_log() {
    let ttl = Duration::from_secs(3);
    let fake = Fake::default();
    fake.lease_expires();
    let t = start_with(
        &fake,
        Settings {
            lease_ttl: ttl,
            ..settings()
        },
    )
    .await;
    seed(&t, "a1", 2).await;
    tokio::time::advance(Duration::from_secs(4)).await;
    t.renew_lease().await;
    assert!(t.lease().retaken);
    // The rebuild after the retake reads while our clock stands still. Another
    // tracker takes the lease and writes; the read then sees its record.
    fake.hold_next_read();
    let writing = tokio::spawn({
        let t = t.clone();
        async move { t.person_record(key("gab"), imported("c1")).await }
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    another_takes_the_lease(&fake).await;
    let theirs = WorkLogged::new(0, key("gab"), Actor::Person, imported("b1"));
    fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
    fake.open_gate();
    let r = writing.await.unwrap();
    assert!(matches!(r, Err(Error::Unavailable(_))), "{r:?}");
    assert_eq!(
        imports_of(&fake, "c1"),
        0,
        "appended after the new holder's record"
    );
}

#[tokio::test]
async fn the_lease_taken_at_start_is_confirmed_after_the_read() {
    let fake = Fake::default();
    // The read after the lease is granted is where our clock stands still,
    // and another tracker takes the lease and writes.
    fake.hold_next_lease_put();
    let starting = tokio::spawn(Tracker::start(fake.reconnect(), settings()));
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    fake.hold_next_read();
    fake.open_gate();
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    another_takes_the_lease(&fake).await;
    let theirs = WorkLogged::new(0, key("gab"), Actor::Person, imported("b1"));
    fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
    fake.open_gate();
    let t = starting.await.unwrap().unwrap();
    let r = t.person_record(key("gab"), imported("c1")).await;
    assert!(matches!(r, Err(Error::Unavailable(_))), "{r:?}");
    assert_eq!(
        imports_of(&fake, "c1"),
        0,
        "appended after the new holder's record"
    );
}

#[tokio::test(start_paused = true)]
async fn a_renewal_that_landed_with_its_reply_lost_does_not_fence_the_holder() {
    let fake = Fake::default();
    fake.lease_expires();
    let t = start(&fake).await;
    t.renew_lease().await;
    // The renewal lands with its reply lost; it finds the entry ours and
    // renews by the version it never heard of.
    fake.lose_next_lease_reply();
    t.renew_lease().await;
    assert!(!t.lease().fenced, "the holder fenced itself");
    t.person_record(key("gab"), imported("c1")).await.unwrap();
    assert_eq!(imports_of(&fake, "c1"), 1);
}

#[tokio::test(start_paused = true)]
async fn a_lease_taken_at_start_whose_reply_was_lost_is_held() {
    let fake = Fake::default();
    fake.lease_expires();
    fake.lose_next_lease_reply();
    let t = Tracker::start(fake.reconnect(), settings())
        .await
        .expect("the start gave up on a lease it holds");
    t.renew_lease().await;
    assert!(!t.lease().fenced);
    t.person_record(key("gab"), imported("c1")).await.unwrap();
    assert_eq!(imports_of(&fake, "c1"), 1);
}

/// A tracker whose lease ran out while another held it, wrote, and stopped.
async fn lapsed_while_another_wrote(fake: &Fake, ttl: Duration) -> Tracker<Fake> {
    let short = || Settings {
        lease_ttl: ttl,
        ..settings()
    };
    fake.lease_expires();
    let first = start_with(fake, short()).await;
    tokio::time::advance(ttl + Duration::from_secs(1)).await;
    let second = Tracker::start(fake.reconnect(), short()).await.unwrap();
    second.renew_lease().await;
    seed(&second, "a1", 2).await;
    tokio::time::advance(ttl + Duration::from_secs(1)).await;
    first
}

#[tokio::test(start_paused = true)]
async fn a_retake_that_landed_with_its_reply_lost_reads_what_another_wrote() {
    let fake = Fake::default();
    let t = lapsed_while_another_wrote(&fake, Duration::from_secs(3)).await;
    fake.lose_next_lease_reply();
    t.renew_lease().await;
    t.renew_lease().await;
    assert!(!t.lease().fenced, "the holder fenced itself");
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
}

#[tokio::test(start_paused = true)]
async fn a_retake_adopted_a_round_late_still_reads_what_another_wrote() {
    let fake = Fake::default();
    let t = lapsed_while_another_wrote(&fake, Duration::from_secs(3)).await;
    // The retake lands with its reply lost, and the lease key cannot be read.
    fake.lose_next_lease_reply();
    fake.fail_lease_gets(true);
    t.renew_lease().await;
    fake.fail_lease_gets(false);
    // The next round finds the entry ours at a version it never heard of.
    t.renew_lease().await;
    {
        let lease = t.lease();
        assert!(!lease.fenced, "the holder fenced itself");
        assert!(
            lease.retaken,
            "an entry taken back was adopted as a renewal"
        );
    }
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
}

#[tokio::test(start_paused = true)]
async fn an_entry_held_by_another_tracker_is_not_adopted() {
    let fake = Fake::default();
    let t = start(&fake).await;
    t.renew_lease().await;
    another_takes_the_lease(&fake).await;
    t.renew_lease().await;
    assert!(t.lease().fenced);
}

#[tokio::test(start_paused = true)]
async fn a_retake_adopted_whose_renewal_reply_is_lost_still_reads_what_another_wrote() {
    let fake = Fake::default();
    let t = lapsed_while_another_wrote(&fake, Duration::from_secs(3)).await;
    // The retake and the renewal by its adopted version both lose their replies.
    fake.lose_next_lease_reply();
    fake.lose_next_lease_reply();
    t.renew_lease().await;
    {
        let lease = t.lease();
        assert!(
            lease.retaken,
            "an entry taken back was adopted as a renewal"
        );
        assert!(lease.stale);
    }
    t.renew_lease().await;
    assert!(!t.lease().fenced, "the holder fenced itself");
    assert_eq!(claimed_id(&claim(&t, "w1").await).as_deref(), Some("a1"));
}

/// A tracker whose renewal landed with its reply lost and could not read the
/// entry back, then whose view ran out: Felix holds the entry for it at a
/// version it never heard of.
async fn ahead_of_our_view(fake: &Fake, ttl: Duration) -> Tracker<Fake> {
    let t = start_with(
        fake,
        Settings {
            lease_ttl: ttl,
            ..settings()
        },
    )
    .await;
    fake.lose_next_lease_reply();
    fake.fail_lease_gets(true);
    t.renew_lease().await;
    fake.fail_lease_gets(false);
    assert!(!t.lease().fenced);
    tokio::time::advance(ttl + Duration::from_secs(1)).await;
    t
}

#[tokio::test(start_paused = true)]
async fn an_adopted_entry_that_runs_out_before_the_renewal_is_taken_again() {
    let fake = Fake::default();
    let t = ahead_of_our_view(&fake, Duration::from_secs(3)).await;
    // The read finds the entry ours, and it runs out before the put by it lands.
    fake.expire_lease_after_next_get();
    t.renew_lease().await;
    let lease = t.lease();
    assert!(!lease.fenced, "fenced though nobody holds the lease");
    assert!(
        lease.retaken,
        "a lease taken back with Absent was not a retake"
    );
    assert!(lease.stale);
    drop(lease);
    assert!(fake.lease_is_held());
}

#[tokio::test(start_paused = true)]
async fn a_lagging_read_of_our_entry_is_refused_and_writes_nothing() {
    let fake = Fake::default();
    let t = ahead_of_our_view(&fake, Duration::from_secs(3)).await;
    let entry = fake.lease_version();
    // The read sees our token at a version older than the entry.
    fake.lease_gets_lag_at(Some(entry.unwrap() - 2));
    t.renew_lease().await;
    assert!(t.lease().fenced);
    assert_eq!(
        fake.lease_version(),
        entry,
        "a stale version or Absent landed"
    );
    let err = t.person_record(key("gab"), imported("c1")).await;
    assert!(err.is_err(), "a fenced tracker wrote");
    assert_eq!(imports_of(&fake, "c1"), 0);
}

/// Another writer's record takes the offset this tracker asked for; on a Raft
/// shard the refusal then burns the next one, and the log ends in a blank.
async fn refused_into_a_trailing_blank(fake: &Fake, t: &Tracker<Fake>) {
    let theirs = WorkLogged::new(0, key("gab"), Actor::Person, imported("b1"));
    fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
    assert!(matches!(
        t.person_record(key("gab"), imported("c1")).await,
        Err(Error::Unavailable(_))
    ));
}

#[tokio::test]
async fn a_raft_blank_at_the_end_of_the_log_does_not_refuse_the_next_write_again() {
    let fake = Fake::default();
    fake.raft_blanks();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    refused_into_a_trailing_blank(&fake, &t).await;
    let blanks = || fake.net.lock().unwrap().blanks[WORK_EVENTS].len();
    assert_eq!(blanks(), 1);
    // The next call rebuilds; its tip is past the blank, so the write lands.
    for _ in 0..3 {
        if t.person_record(key("gab"), imported("c1")).await.is_ok() {
            break;
        }
    }
    assert_eq!(imports_of(&fake, "c1"), 1);
    assert_eq!(blanks(), 1, "the retry burned another offset");
    // And the ones after it, without a rebuild in between.
    t.person_record(key("gab"), imported("d1")).await.unwrap();
    assert_eq!(imports_of(&fake, "d1"), 1);
    assert_eq!(blanks(), 1);
}

#[tokio::test]
async fn a_restart_over_a_trailing_raft_blank_writes_past_it() {
    let fake = Fake::default();
    fake.raft_blanks();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    refused_into_a_trailing_blank(&fake, &t).await;
    // A new tracker reads the log from scratch: record, record, blank.
    let t2 = start(&fake).await;
    t2.person_record(key("gab"), imported("c1")).await.unwrap();
    assert_eq!(imports_of(&fake, "c1"), 1);
    assert_eq!(fake.net.lock().unwrap().blanks[WORK_EVENTS].len(), 1);
}

#[tokio::test]
async fn refusals_that_burn_offsets_in_a_row_each_settle_on_the_next_write() {
    let fake = Fake::default();
    fake.raft_blanks();
    let t = start(&fake).await;
    seed(&t, "a1", 2).await;
    for i in 0..3 {
        // A leader change leaves a blank, and another writer lands a record.
        fake.blank(WORK_EVENTS);
        let theirs = WorkLogged::new(0, key("gab"), Actor::Person, imported(&format!("b{i}")));
        fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
        let mut landed = false;
        for _ in 0..4 {
            if t.person_record(key("gab"), imported(&format!("c{i}")))
                .await
                .is_ok()
            {
                landed = true;
                break;
            }
        }
        assert!(landed, "write {i} never landed past the blank");
    }
    assert_eq!(imports_of(&fake, "c2"), 1);
}

#[tokio::test]
async fn history_pages_over_a_run_of_blanks() {
    let fake = Fake::default();
    fake.raft_blanks();
    let t = start(&fake).await;
    seed(&t, "a1", 1).await;
    // More blanks than a page holds, between the records.
    for _ in 0..7 {
        fake.blank(WORK_EVENTS);
    }
    let theirs = WorkLogged::new(0, key("gab"), Actor::Person, imported("b1"));
    fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
    let _ = t.person_record(key("gab"), imported("a1")).await;
    fake.page_size(2);
    let h = t.history(&item("a1"), 0).await.unwrap();
    let offsets: Vec<u64> = h["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["offset"].as_u64().unwrap())
        .collect();
    assert!(offsets.windows(2).all(|w| w[0] < w[1]), "{offsets:?}");
    assert!(!offsets.is_empty());
    assert_eq!(h["next"], serde_json::Value::Null);
}

/// The fold of the log to any offset, with or without snapshots, is the
/// state the tracker held once it had applied that record, blanks included.
#[tokio::test]
async fn the_fold_to_an_offset_is_the_state_the_tracker_held_then() {
    let fake = Fake::default();
    fake.raft_blanks();
    let t = start(&fake).await;
    let mut held: Vec<(u64, State)> = Vec::new();
    for (i, id) in ["a1", "b1", "c1", "d1"].into_iter().enumerate() {
        // A leader change burns an offset before every other write.
        if i % 2 == 1 {
            fake.blank(WORK_EVENTS);
        }
        // The first write after a blank is refused once, then lands past it.
        let mut landed = false;
        for _ in 0..4 {
            landed = t.person_record(key("gab"), imported(id)).await.is_ok();
            if landed {
                break;
            }
        }
        assert!(landed, "{id} never landed");
        let state = t.0.core.lock().await.state.clone();
        held.push((state.at.unwrap(), state));
    }
    let log: Vec<(u64, WorkLogged)> = fake.net.lock().unwrap().streams[WORK_EVENTS]
        .iter()
        .map(|(o, p)| (*o, WorkLogged::decode(p).unwrap()))
        .collect();
    assert!(log.windows(2).any(|w| w[1].0 > w[0].0 + 1), "no blank");
    let snaps = kbl_core::work_fold::Snapshots::build_every(&log, 3);
    for n in 0..=log.last().unwrap().0 + 1 {
        let want = held
            .iter()
            .rev()
            .find(|(o, _)| *o <= n)
            .map_or_else(State::default, |(_, s)| s.clone());
        assert_eq!(kbl_core::work_fold::fold_to(&log, n), want, "at {n}");
        assert_eq!(snaps.fold_to(&log, n), want, "from snapshots, at {n}");
    }
}
