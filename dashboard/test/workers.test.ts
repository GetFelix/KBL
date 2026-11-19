import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { test } from 'node:test';
import { COMMANDS } from '../src/keys.ts';
import { Tracker, type Fetch } from '../src/tracker.ts';
import { groups, launchText, liveNames, stateOf, stopEach, stopRequest, summary, workers, type WorkerRow } from '../src/workers.ts';
import { items } from '../src/fold.ts';
import type { Outcome } from '../src/writes.ts';

// Built by `npm run wasm`.
const { Folder } = createRequire(import.meta.url)('../.wasm-node/kbl_wasm.js');

const HASH = (c: string) => c.repeat(64);

/** Two supervisors: `sup` with a working, an idle, a finished and a stopped worker, and `other` with one idle. */
function folded() {
  const fold = new Folder();
  let at = 0;
  let ms = 1_000;
  const apply = (o: object) => assert.ok(fold.apply(at++, new TextEncoder().encode(JSON.stringify({ at_ms: (ms += 1000), ...o }))), JSON.stringify(o));
  const person = (o: object) => apply({ by: 'gabriel', actor: 'person', ...o });
  const agent = (by: string, o: object) => apply({ v: 24, by, actor: 'agent', ...o });
  const as = (by: string, sup: string, o: object) => apply({ v: 24, by, sup, actor: 'agent', ...o });
  person({ v: 24, kind: 'format_raised', to: 24 });
  apply({ kind: 'imported', item: 'r1', source: 'github', upstream_id: 'GetFelix/felix#1', title: 'Fix the lease', actor: 'tracker', by: 'ingest' });
  person({ kind: 'accepted', item: 'r1', tier: 1 });
  const start = (by: string, worker: string, hash: string, extra: object = {}) =>
    agent(by, { kind: 'worker_started', worker, token_sha256: HASH(hash), supervisor_token_sha256: HASH('f'), ...extra });
  start('sup', 'sup-a', 'a', { vendor: 'anthropic', model: 'sonnet' });
  start('sup', 'sup-b', 'b', { vendor: 'example', profile: 'cloud' });
  start('sup', 'sup-c', 'c');
  start('sup', 'sup-d', 'd');
  start('other', 'other-x', 'e');
  as('sup-a', 'sup', { kind: 'claimed', item: 'r1', attempt: 1, deadline_ms: 1800000090000 });
  as('sup-c', 'sup', { kind: 'worker_finished' });
  person({ v: 24, kind: 'worker_stopped', worker: 'sup-d' });
  assert.deepEqual(JSON.parse(fold.state_json()).rejections, []);
  return fold;
}

test('workers_json lists every worker with what it holds, and no token hash', () => {
  const rows = workers({ workersJson: () => folded().workers_json() });
  assert.deepEqual(rows.map((w) => [w.worker, w.supervisor, stateOf(w)]), [
    ['other-x', 'other', 'idle'],
    ['sup-a', 'sup', 'working'],
    ['sup-b', 'sup', 'idle'],
    ['sup-c', 'sup', 'finished'],
    ['sup-d', 'sup', 'stopped'],
  ]);
  assert.deepEqual(rows[1].holds, { item: 'r1', attempt: 1 });
  assert.equal(rows[2].holds, null);
  assert.ok(!folded().workers_json().includes(HASH('a')));
});

test('a group is a supervisor with live workers first, and says how the pool ended', () => {
  const fold = folded();
  const gs = groups(workers({ workersJson: () => fold.workers_json() }), items({ itemsJson: () => fold.items_json(), format: () => 24 } as never));
  assert.deepEqual(gs.map((g) => [g.supervisor, g.live]), [['other', 1], ['sup', 2]]);
  const sup = gs[1];
  assert.deepEqual(sup.workers.map((w) => w.state), ['working', 'idle', 'finished', 'stopped']);
  assert.equal(summary(sup), '1 working, 1 idle, 1 finished, 1 stopped');
  assert.deepEqual(sup.workers[0].hold && [sup.workers[0].hold.item, sup.workers[0].hold.title, sup.workers[0].hold.stage], ['r1', 'Fix the lease', 'held']);
  assert.deepEqual(liveNames(gs, 'sup'), ['sup-a', 'sup-b']);
  assert.deepEqual(liveNames(gs, null), ['other-x', 'sup-a', 'sup-b']);
  assert.deepEqual(liveNames(gs, 'nobody'), []);
});

test('a stopped or finished worker no longer holds, and a worker that holds a missing item still shows it', () => {
  const row = (o: Partial<WorkerRow>): WorkerRow => ({ worker: 's-a', supervisor: 's', vendor: null, profile: null, model: null, started_ms: 1, stopped_ms: null, finished: false, holds: null, ...o });
  const [g] = groups([row({ holds: { item: 'gone', attempt: 2 } })], []);
  assert.deepEqual(g.workers[0].hold, { item: 'gone', attempt: 2, title: null, stage: null, active_ms: null });
  assert.equal(stateOf(row({ stopped_ms: 5, finished: true })), 'finished');
  assert.equal(stateOf(row({ stopped_ms: 5 })), 'stopped');
});

test('launch text shows only what the supervisor recorded', () => {
  assert.equal(launchText({ vendor: 'anthropic', profile: 'claude', model: 'sonnet' }), 'anthropic · sonnet');
  assert.equal(launchText({ vendor: 'example', profile: 'cloud', model: null }), 'example · cloud');
  assert.equal(launchText({ vendor: null, profile: null, model: null }), '');
});

test('stop sends the worker to the stop route with the viewer token, under the team when one is named', async () => {
  const seen: { url: string; method: string; auth: string; body: unknown }[] = [];
  const fetch: Fetch = async (url, init) => {
    seen.push({ url, method: init.method, auth: init.headers.authorization, body: JSON.parse(init.body ?? 'null') });
    return { status: 200, json: async () => ({ kind: 'worker_stopped', offset: 9 }) };
  };
  assert.deepEqual(stopRequest('sup-a'), { path: '/v1/work/workers/stop', body: { worker: 'sup-a' } });
  assert.equal((await new Tracker(fetch, 'tok', null).write(stopRequest('sup-a'))).status, 'ok');
  assert.equal((await new Tracker(fetch, 'tok', 'core team').write(stopRequest('sup-b'))).status, 'ok');
  assert.deepEqual(seen, [
    { url: '/v1/work/workers/stop', method: 'POST', auth: 'Bearer tok', body: { worker: 'sup-a' } },
    { url: '/v1/teams/core%20team/work/workers/stop', method: 'POST', auth: 'Bearer tok', body: { worker: 'sup-b' } },
  ]);
});

test('a stop the tracker refuses says why', async () => {
  const fetch: Fetch = async () => ({ status: 409, json: async () => ({ error: 'worker is not live', why: 'bad_worker' }) });
  const o = await new Tracker(fetch, 'tok', null).write(stopRequest('sup-a'));
  assert.equal(o.status, 'refused');
  assert.equal(o.status === 'refused' && o.why, 'bad_worker');
});

test('stopping several workers skips one already stopped and reports the first real refusal', async () => {
  const sent: string[] = [];
  const answers: Record<string, Outcome> = {
    a: { status: 'ok', offset: 1 } as Outcome,
    b: { status: 'refused', why: 'bad_worker', reason: 'not live' },
    c: { status: 'refused', why: 'forbidden', reason: 'This token may not make that write.' },
  };
  const send = async (r: { body: Record<string, unknown> }) => (sent.push(String(r.body.worker)), answers[String(r.body.worker)]);
  assert.equal(await stopEach(send, ['a', 'b']), null);
  sent.length = 0;
  assert.equal(await stopEach(send, ['a', 'b', 'c', 'a']), 'c: This token may not make that write.');
  assert.deepEqual(sent, ['a', 'b', 'c'], 'nothing is sent after the refusal');
});

test('Workers is a palette command', () => {
  assert.ok(COMMANDS.some((c) => c.id === 'workers'));
});
