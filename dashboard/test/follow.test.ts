import assert from 'node:assert/strict';
import { test } from 'node:test';
import { follow, type Sink } from '../src/follow.ts';
import { FakeGateway, record } from './fake-gateway.ts';

function recorder() {
  const seen: { offset: number; n: number }[] = [];
  const sink: Sink = {
    apply(offset, payload) {
      seen.push({ offset, n: JSON.parse(new TextDecoder().decode(payload)).n });
      return true;
    },
  };
  return { seen, sink };
}

const settle = () => new Promise((r) => setTimeout(r, 5));

function gatewayWith(count: number) {
  const gw = new FakeGateway();
  for (let n = 0; n < count; n++) gw.publish(record(n));
  return gw;
}

test('history then live: every record once, in order', async () => {
  const gw = gatewayWith(5);
  const { seen, sink } = recorder();
  let live = false;
  const done = follow(gw, sink, { stream: 'events', onLive: () => (live = true) });
  await settle();
  assert.ok(live);
  gw.publish(record(5));
  await settle();
  assert.deepEqual(seen.map((s) => s.offset), [0, 1, 2, 3, 4, 5]);
  gw.close();
  assert.equal(await done, 6);
});

test('a publish between the read and the subscribe arrives exactly once', async () => {
  const gw = gatewayWith(5);
  gw.afterEmptyRead = () => {
    gw.publish(record(5)); // lands after the last read page, before the subscribe registers
  };
  const { seen, sink } = recorder();
  const done = follow(gw, sink, { stream: 'events' });
  await settle();
  gw.publish(record(6));
  await settle();
  assert.deepEqual(seen.map((s) => s.offset), [0, 1, 2, 3, 4, 5, 6]);
  assert.deepEqual(seen.map((s) => s.n), [0, 1, 2, 3, 4, 5, 6]);
  assert.deepEqual(gw.subscribedFrom, [5], 'subscribed from the offset after the last record read');
  gw.close();
  await done;
});

test('an event the page already applied is dropped', async () => {
  const gw = gatewayWith(3);
  const { seen, sink } = recorder();
  const done = follow(gw, sink, { stream: 'events' });
  await settle();
  gw.onMessage({ type: 'event', offset: 2, payload: record(2) });
  gw.onMessage({ type: 'event', offset: 0, payload: record(0) });
  await settle();
  assert.deepEqual(seen.map((s) => s.offset), [0, 1, 2]);
  gw.close();
  await done;
});

test('a dropped live event is refilled by subscribing again from the cursor', async () => {
  const gw = gatewayWith(2);
  const { seen, sink } = recorder();
  const done = follow(gw, sink, { stream: 'events' });
  await settle();
  const lost = new Set([3]); // the subscription loses offset 3 once
  gw.filterEvent = (o) => !lost.delete(o);
  gw.publish(record(2));
  gw.publish(record(3));
  gw.publish(record(4));
  await settle();
  assert.deepEqual(seen.map((s) => s.offset), [0, 1, 2, 3, 4]);
  assert.deepEqual(gw.subscribedFrom, [2, 3]);
  gw.close();
  await done;
});

test('an empty log goes live from offset 0', async () => {
  const gw = new FakeGateway();
  const { seen, sink } = recorder();
  const done = follow(gw, sink, { stream: 'events' });
  await settle();
  gw.publish(record(0));
  await settle();
  assert.deepEqual(seen.map((s) => s.offset), [0]);
  assert.deepEqual(gw.subscribedFrom, [0]);
  gw.close();
  await done;
});

test('a gateway error ends the follow', async () => {
  const gw = gatewayWith(1);
  const { sink } = recorder();
  const done = follow(gw, sink, { stream: 'events' });
  gw.onMessage({ type: 'error', code: 'trimmed', message: 'oldest 10' });
  await assert.rejects(done, /trimmed/);
});

test('events of the replaced subscription that arrive before the new subscribed do not loop', async () => {
  const gw = gatewayWith(2);
  const { seen, sink } = recorder();
  const done = follow(gw, sink, { stream: 'events' });
  await settle();
  const lost = new Set([3]);
  gw.filterEvent = (o) => !lost.delete(o);
  // Stale events in flight when the page resubscribes: a gap again (offset 6 after 4).
  gw.beforeSubscribed = () => {
    gw.beforeSubscribed = () => {};
    gw.onMessage({ type: 'event', offset: 6, payload: record(6) });
  };
  gw.publish(record(2));
  gw.publish(record(3));
  gw.publish(record(4));
  await settle();
  assert.deepEqual(gw.subscribedFrom, [2, 3], 'one resubscribe, not a loop');
  assert.deepEqual(seen.map((s) => s.offset), [0, 1, 2, 3, 4]);
  gw.close();
  await done;
});

test('an error after the read ends the follow with the gateway code', async () => {
  const gw = gatewayWith(4);
  gw.readError = (from) => (from >= 2 ? { type: 'error', code: 'trimmed', message: 'oldest 10' } : undefined);
  const { seen, sink } = recorder();
  await assert.rejects(follow(gw, sink, { stream: 'events' }), /trimmed/);
  assert.deepEqual(seen.map((s) => s.offset), [0, 1]);
  assert.ok(gw.closed);
});

test('a reconnect with from:N reads and subscribes from N', async () => {
  const gw = gatewayWith(6);
  const { seen, sink } = recorder();
  const done = follow(gw, sink, { stream: 'events', from: 4 });
  await settle();
  assert.deepEqual(seen.map((s) => s.offset), [4, 5]);
  assert.deepEqual(gw.subscribedFrom, [6]);
  gw.close();
  assert.equal(await done, 6);
});

test('an empty page whose next_offset is ahead moves the cursor and subscribes there', async () => {
  const gw = gatewayWith(2);
  gw.emptyPageAhead = 3; // e.g. records that hold no event at the end of the range
  const { seen, sink } = recorder();
  const done = follow(gw, sink, { stream: 'events' });
  await settle();
  assert.deepEqual(gw.subscribedFrom, [5]);
  assert.deepEqual(seen.map((s) => s.offset), [0, 1]);
  gw.close();
  assert.equal(await done, 5);
});

test('skipped_before explains a jump: no resubscribe', async () => {
  const gw = gatewayWith(2);
  const { seen, sink } = recorder();
  const done = follow(gw, sink, { stream: 'events' });
  await settle();
  gw.nonEvents.add(2); // a generation-start record takes offset 2 and is never delivered
  gw.publish(record(2));
  gw.publish(record(3));
  await settle();
  assert.deepEqual(seen.map((s) => s.offset), [0, 1, 3]);
  assert.deepEqual(gw.subscribedFrom, [2], 'no resubscribe');
  gw.close();
  await done;
});

test('records the sink refuses are counted and skipped', async () => {
  const gw = gatewayWith(4);
  const counts: number[] = [];
  const sink: Sink = { apply: (offset) => offset !== 1 && offset !== 2 };
  const done = follow(gw, sink, { stream: 'events', onRefused: (n) => counts.push(n) });
  await settle();
  assert.deepEqual(counts, [1, 2]);
  gw.close();
  assert.equal(await done, 4);
});

test('a record of a newer format ends the follow instead of being skipped', async () => {
  const gw = gatewayWith(4);
  const applied: number[] = [];
  const sink: Sink = {
    apply: (offset) => offset < 2 && applied.push(offset) > 0,
    newer: () => (applied.length === 2 ? 2 : 0),
  };
  await assert.rejects(follow(gw, sink, { stream: 'events' }), /record 2 has format 2.*Reload/);
  assert.deepEqual(applied, [0, 1]);
});

test('a payload that is not base64 ends the follow', async () => {
  const gw = gatewayWith(1);
  gw.log.push('***not base64***');
  const { sink } = recorder();
  await assert.rejects(follow(gw, sink, { stream: 'events' }), /not base64/);
});

test('subscription_ended resubscribes from the cursor and the log carries on', async () => {
  const gw = gatewayWith(3);
  const { seen, sink } = recorder();
  const done = follow(gw, sink, { stream: 'events' });
  await settle();
  gw.onMessage({ type: 'error', code: 'subscription_ended', message: 'broker failover' });
  gw.publish(record(3)); // lands while the page is resubscribing; the new subscription delivers it
  await settle();
  gw.publish(record(4));
  await settle();
  assert.deepEqual(seen.map((s) => s.offset), [0, 1, 2, 3, 4]);
  assert.deepEqual(gw.subscribedFrom, [3, 3], 'subscribed again from the cursor');
  assert.ok(!gw.closed);
  gw.close();
  await done;
});

test('a transient gateway error hands the cursor back instead of stopping', async () => {
  const gw = gatewayWith(4);
  gw.readError = (from) => (from >= 2 ? { type: 'error', code: 'read_failed', message: 'broker busy' } : undefined);
  const { seen, sink } = recorder();
  assert.equal(await follow(gw, sink, { stream: 'events' }), 2);
  assert.deepEqual(seen.map((s) => s.offset), [0, 1]);
});

test('a stale event does not move the live position back (spurious resubscribe)', async () => {
  const gw = gatewayWith(2);
  const { seen, sink } = recorder();
  const done = follow(gw, sink, { stream: 'events' });
  await settle();
  gw.publish(record(2));
  gw.publish(record(3));
  gw.onMessage({ type: 'event', offset: 1, payload: record(1) }); // already applied
  gw.publish(record(4));
  await settle();
  assert.deepEqual(seen.map((s) => s.offset), [0, 1, 2, 3, 4]);
  assert.deepEqual(gw.subscribedFrom, [2], 'no resubscribe');
  gw.close();
  await done;
});

test('a read page that repeats records already applied does not apply them twice', async () => {
  const gw = gatewayWith(6);
  gw.overlap = 1;
  const { seen, sink } = recorder();
  const done = follow(gw, sink, { stream: 'events' });
  await settle();
  assert.deepEqual(seen.map((s) => s.offset), [0, 1, 2, 3, 4, 5]);
  gw.close();
  await done;
});
