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
