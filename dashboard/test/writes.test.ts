import assert from 'node:assert/strict';
import test from 'node:test';
import type { ItemRow } from '../src/fold.ts';
import { reduce, type ViewState } from '../src/state.ts';
import { Tracker, type Fetch } from '../src/tracker.ts';
import { CAN, checkAnswer, latestFor, NO_WRITES, outcome, pending, putWrite, request, route, summary, undoFor, undoRequest, type Write } from '../src/writes.ts';

const item = (o: Partial<ItemRow> & { id: string }): ItemRow => ({ title: o.id, stage: 'ready', tier: 1, ...o });

test('each action is offered only where the fold would take it', () => {
  const triage = item({ id: 't', stage: 'triage', tier: null });
  const proposed = item({ id: 'p', stage: 'proposed' });
  const done = item({ id: 'd', stage: 'done' });
  const asked = item({ id: 'q', stage: 'needs_you', needs: 'question', question: 7 });
  const failed = item({ id: 'f', stage: 'needs_you', needs: 'check_failed' });
  assert.deepEqual([CAN.accept(triage), CAN.accept(proposed), CAN.accept(done)], [true, true, false]);
  assert.deepEqual([CAN.reject(triage), CAN.reject(proposed)], [false, true]);
  assert.deepEqual([CAN.close(done), CAN.tier(done), CAN.close(asked)], [false, false, true]);
  assert.deepEqual([CAN.answer(asked), CAN.answer(failed)], [true, false]);
});

test('requests are the tracker person routes, under the team when one is named', () => {
  assert.deepEqual(request('a', { kind: 'accept', tier: 2 }), { path: '/v1/work/proposals/accept', body: { item: 'a', tier: 2 } });
  assert.deepEqual(request('a', { kind: 'close', why: 'dismissed' }), { path: '/v1/work/close', body: { item: 'a', why: 'dismissed' } });
  assert.deepEqual(request('a', { kind: 'answer', question: 4, text: '  ', chosen: 'rust' }), {
    path: '/v1/work/questions/answer',
    body: { item: 'a', question: 4, chosen: 'rust' },
  });
  assert.equal(route('/v1/work/tier', null), '/v1/work/tier');
  assert.equal(route('/v1/work/tier', 'a b'), '/v1/teams/a%20b/work/tier');
});

test('an answer form names what is missing before anything is sent', () => {
  assert.equal(checkAnswer([], '', null), 'Write an answer.');
  assert.equal(checkAnswer(['a', 'b'], '', null), 'Pick an option, or write an answer.');
  assert.equal(checkAnswer(['a'], '', 'a'), null);
  assert.equal(checkAnswer(['a'], '', 'gone'), 'That option is no longer offered.');
  assert.match(checkAnswer([], 'x'.repeat(5000), null)!, /4 KiB/);
});

test('a refusal keeps the tracker’s own reason and code', () => {
  assert.deepEqual(outcome(200, { kind: 'accepted', offset: 12 }), { status: 'ok', offset: 12 });
  assert.deepEqual(outcome(409, { error: 'the fold refused the record: not a proposal', why: 'not_a_proposal' }), {
    status: 'refused',
    why: 'not_a_proposal',
    reason: 'the fold refused the record: not a proposal',
  });
  assert.equal((outcome(401, null) as { why: string }).why, 'signed_out');
  assert.equal(outcome(503, { error: 'team busy' }).status, 'failed');
  assert.match((outcome(null, null, 'Failed to fetch') as { reason: string }).reason, /Cannot reach the tracker: Failed to fetch/);
});

test('only a tier change can be undone, and only while nobody changed it again', () => {
  const before = item({ id: 'a', tier: 1 });
  const u = undoFor(before, 3);
  assert.deepEqual(u, { item: 'a', tier: 1, set: 3 });
  assert.match(undoFor(item({ id: 'b', tier: null }), 2) as string, /no record removes a tier/);
  if (typeof u === 'string') throw new Error('expected an undo');
  // The undo is a new tier_changed record setting the old tier back.
  assert.deepEqual(undoRequest(u, [item({ id: 'a', tier: 3 })]), { path: '/v1/work/tier', body: { item: 'a', tier: 1 } });
  assert.match(undoRequest(u, [item({ id: 'a', tier: 0 })]) as string, /changed again since/);
  assert.match(undoRequest(u, [item({ id: 'a', tier: 3, stage: 'closed' })]) as string, /closed now/);
  assert.match(undoRequest(u, []) as string, /no longer in the log/);
});

test('the write list moves from pending to a result and says so in one line', () => {
  const w = (o: Partial<Write> & { id: number; item: string }): Write => ({ batch: 1, kind: 'tier', status: 'pending', ...o });
  let ws = putWrite(NO_WRITES, w({ id: 1, item: 'a', undo: { item: 'a', tier: 1, set: 2 } }));
  ws = putWrite(ws, w({ id: 2, item: 'b' }));
  assert.equal(pending(ws, 'a'), true);
  assert.deepEqual(summary(ws.list), { text: 'Setting the tier of 2 items…', tone: 'busy', undoable: false });
  ws = putWrite(ws, { ...ws.list[0], status: 'ok' });
  ws = putWrite(ws, { ...ws.list[1], status: 'refused', reason: 'the item is closed', why: 'closed' });
  assert.equal(pending(ws, 'a'), false);
  assert.equal(latestFor(ws, 'b')?.status, 'refused');
  assert.deepEqual(summary(ws.list), { text: 'Set the tier of a. b: refused, the item is closed (closed)', tone: 'error', undoable: true });
  assert.equal(ws.last, 1);
});

test('marks toggle, and a mark on an item the log no longer has is dropped', () => {
  const s = { marked: [], items: [item({ id: 'a' }), item({ id: 'b' })], writes: NO_WRITES } as unknown as ViewState;
  let n = reduce(s, { type: 'mark', id: 'a' });
  n = reduce(n, { type: 'mark', id: 'b' });
  assert.deepEqual(n.marked, ['a', 'b']);
  n = reduce(n, { type: 'mark', id: 'a' });
  assert.deepEqual(n.marked, ['b']);
  n = reduce(n, { type: 'items', items: [item({ id: 'a' })], blocked: [], overlaps: new Map() });
  assert.deepEqual(n.marked, []);
});

test('the client sends the viewer’s token to the page’s own routes, and reads refusals', async () => {
  const seen: { url: string; auth: string; body?: string }[] = [];
  const fake: Fetch = async (url, init) => {
    seen.push({ url, auth: init.headers.authorization, body: init.body });
    if (url === '/v1/me') return { status: 200, json: async () => ({ person: 'sam', teams: [{ team: 'default', role: 'admin' }] }) };
    return { status: 409, json: async () => ({ error: 'not the open question', why: 'not_open_question' }) };
  };
  const t = new Tracker(fake, 'tok', 'default');
  assert.deepEqual(await t.me(), { person: 'sam', teams: ['default'] });
  const o = await t.write(request('q', { kind: 'answer', question: 3, text: 'yes', chosen: null }));
  assert.deepEqual(o, { status: 'refused', why: 'not_open_question', reason: 'not the open question' });
  assert.deepEqual(seen.map((x) => [x.url, x.auth]), [['/v1/me', 'Bearer tok'], ['/v1/teams/default/work/questions/answer', 'Bearer tok']]);
  const down = new Tracker(async () => Promise.reject(new Error('Failed to fetch')), 'tok', null);
  assert.equal((await down.write(request('a', { kind: 'reject' }))).status, 'failed');
});

test('the Needs you inbox is a view in the address that holds only items waiting on a person', async () => {
  const { decodeView, encodeView } = await import('../src/views.ts');
  const { inScope } = await import('../src/list.ts');
  assert.equal(decodeView('view=inbox').view, 'inbox');
  assert.equal(encodeView('', { view: 'inbox', layout: 'list', query: '', filters: {} }), 'view=inbox');
  const items = [item({ id: 'a', stage: 'needs_you', needs: 'question' }), item({ id: 'b' })];
  assert.deepEqual(inScope({ items, blocked: [], view: 'inbox' }).map((i) => i.id), ['a']);
});
