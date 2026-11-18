import assert from 'node:assert/strict';
import { test } from 'node:test';
import { adviceFor, createStore, describe, fromAddress, reduce, reviewFull, toAddress, type ViewState } from '../src/state.ts';
import { NO_WRITES } from '../src/writes.ts';

const base: ViewState = { connection: { kind: 'setup' }, synced: false, items: [], blocked: [], overlaps: new Map(), stories: new Map(), reviewLoad: { load: 0, inReview: 0 }, view: 'all', query: '', selected: null, open: null, theme: 'dark', layout: 'list', filters: {}, density: 'comfortable', saved: [], marked: [], writes: NO_WRITES, tracker: { kind: 'signed-out' }, questions: new Map(), kind: null, waiting: new Map() };

test('opening an item selects it, and closing keeps the selection', () => {
  const opened = reduce(base, { type: 'open', id: 'r1' });
  assert.equal(opened.selected, 'r1');
  assert.equal(reduce(opened, { type: 'open', id: null }).selected, 'r1');
});

test('an open item missing from the log stays open so the panel can say so', () => {
  const s = reduce({ ...base, open: 'r9' }, { type: 'items', items: [{ id: 'r1', title: 't', stage: 'ready' }], blocked: [], overlaps: new Map() });
  assert.equal(s.open, 'r9');
});

test('the address round-trips the search and the open item and keeps other parameters', () => {
  const view = { view: 'blocked' as const, query: 'cache', open: 'r1', layout: 'board' as const, filters: { tier: '1' } };
  const search = toAddress('?room=kbl&gateway=wss%3A%2F%2Fg', view);
  assert.deepEqual(fromAddress(search), view);
  assert.ok(search.includes('room=kbl'));
  assert.equal(toAddress('?q=x&item=r1&view=blocked&layout=board&tier=1', { view: 'all', query: '', open: null, layout: 'list', filters: {} }), '');
});

test('opening a saved view replaces the view, layout, search and filters, and keeps the open item', () => {
  const s = reduce({ ...base, query: 'old', filters: { repo: 'a/b' }, open: 'r1' }, { type: 'openView', search: 'view=blocked&layout=board&tier=2' });
  assert.deepEqual([s.view, s.layout, s.query, s.filters, s.open], ['blocked', 'board', '', { tier: '2' }, 'r1']);
  const f = reduce(reduce(base, { type: 'filter', facet: 'source', value: 'github' }), { type: 'filter', facet: 'tier', value: '1' });
  assert.deepEqual(f.filters, { source: 'github', tier: '1' });
  assert.deepEqual(reduce(f, { type: 'clearFilters' }).filters, {});
});

test('the status line says what the page is doing and what failed', () => {
  assert.deepEqual(describe({ kind: 'live', refused: 0 }, 1), { text: 'Live, 1 item', tone: 'ok' });
  assert.equal(describe({ kind: 'live', refused: 2 }, 3).tone, 'warn');
  assert.match(describe({ kind: 'retrying', reason: 'closed', seconds: 4 }, 0).text, /Retrying in 4 s/);
  assert.equal(describe({ kind: 'stopped', reason: 'signed_out: expired', advice: 'sign-in' }, 0).tone, 'error');
});

test('the store hands each new state to its subscribers', () => {
  const store = createStore(base);
  let calls = 0;
  store.subscribe(() => calls++);
  store.dispatch({ type: 'query', query: 'x' });
  assert.equal(calls, 1);
  assert.equal(store.get().query, 'x');
});

test('the page counts as synced only once it has gone live, and stays so across reconnects', () => {
  let s = reduce(base, { type: 'connection', connection: { kind: 'retrying', reason: 'closed', seconds: 1 } });
  assert.equal(s.synced, false);
  s = reduce(s, { type: 'connection', connection: { kind: 'live', refused: 0 } });
  assert.equal(s.synced, true);
  assert.equal(reduce(s, { type: 'connection', connection: { kind: 'loading', what: 'reconnecting' } }).synced, true);
});

test('a stop suggests a sign-in only for a rejected or expired token, and a reload only for a newer format', () => {
  assert.equal(adviceFor('signed_out'), 'sign-in');
  assert.equal(adviceFor('forbidden'), 'sign-in');
  assert.equal(adviceFor('newer_format'), 'reload');
  for (const code of ['trimmed', 'bad_request', 'past_end', 'bad_frame', 'no_read']) assert.equal(adviceFor(code), null, code);
});

test('the address keeps the Blocked view and drops the default one', () => {
  const plain = { query: '', layout: 'list' as const, filters: {} };
  const search = toAddress('?room=kbl', { ...plain, view: 'blocked', open: 'r1' });
  assert.equal(fromAddress(search).view, 'blocked');
  assert.equal(toAddress('?view=blocked', { ...plain, view: 'all', open: null }), '');
});

test('an old ?view=board link opens the board in all work, and the next write moves it to layout', () => {
  const old = fromAddress('?room=kbl&view=board&tier=1');
  assert.deepEqual([old.view, old.layout, old.filters], ['all', 'board', { tier: '1' }]);
  assert.equal(toAddress('?room=kbl&view=board&tier=1', old), '?room=kbl&tier=1&layout=board');
});

test('the view action switches the view and keeps the filters, search and layout', () => {
  const s = reduce({ ...base, query: 'x', filters: { tier: '1' }, layout: 'board' }, { type: 'view', view: 'blocked' });
  assert.deepEqual([s.view, s.query, s.filters, s.layout], ['blocked', 'x', { tier: '1' }, 'board']);
});

test('an items update replaces the overlaps with the ones from the same fold', () => {
  const card = { notes: [{ other: 'r2', files: ['a.rs'], moreFiles: 0, suggest: 'r2 waits on r1' }], more: 0 };
  const s = reduce(base, { type: 'items', items: [], blocked: [], overlaps: new Map([['r1', card]]) });
  assert.equal(s.overlaps.get('r1'), card);
  assert.equal(reduce(s, { type: 'items', items: [], blocked: [], overlaps: new Map() }).overlaps.size, 0);
});

test('the header counts what the pause counts: In review plus held fixes', () => {
  assert.equal(reviewFull({ load: 3, inReview: 2 }, 3), 'review full: agents paused, 3/3: 2 in review, 1 in progress');
  assert.equal(reviewFull({ load: 3, inReview: 0 }, 3), 'review full: agents paused, 3/3: 3 in progress');
  assert.equal(reviewFull({ load: 4, inReview: 4 }, 3), 'review full: agents paused, 4/3: 4 in review');
  assert.equal(reviewFull({ load: 2, inReview: 1 }, 3), null);
  assert.equal(reviewFull({ load: 9, inReview: 9 }, 0), null);
});

test('child cards and open children are separate viewer choices', () => {
  const on = reduce(base, { type: 'childCards', on: true });
  assert.equal(on.childCards, true);
  const open = reduce(on, { type: 'expand', id: 'r1' });
  assert.deepEqual(open.expanded, ['r1']);
  assert.deepEqual(reduce(open, { type: 'expand', id: 'r1' }).expanded, []);
});

test('switching items drops the previous item\'s activity until the next read', () => {
  const act = new Map([['r1', [{ offset: 1, at_ms: 0, by: 'w', kind: 'claimed' }]]]);
  const open = { ...base, open: 'r1', activity: act };
  assert.equal(reduce(open, { type: 'open', id: 'r2' }).activity, undefined);
  assert.equal(reduce(open, { type: 'open', id: 'r1' }).activity, act);
});

test('an items update carries the handoffs of the same fold and keeps the last when one is not sent', () => {
  const ho = { attempt: 1, summary: null, files: 0, added: 0, removed: 0, partial: false, failed_steps: [], failed_total: 0, accurate: null };
  const sent = reduce(base, { type: 'items', items: [], blocked: [], overlaps: new Map(), handoffs: new Map([['r1', ho]]) });
  assert.equal(sent.handoffs?.get('r1')?.attempt, 1);
  assert.equal(reduce(sent, { type: 'items', items: [], blocked: [], overlaps: new Map() }).handoffs?.get('r1')?.attempt, 1);
});
