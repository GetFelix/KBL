import assert from 'node:assert/strict';
import { test } from 'node:test';
import { adviceFor, createStore, describe, fromAddress, reduce, toAddress, type ViewState } from '../src/state.ts';

const base: ViewState = { connection: { kind: 'setup' }, synced: false, items: [], blocked: [], view: 'all', query: '', selected: null, open: null, theme: 'dark' };

test('opening an item selects it, and closing keeps the selection', () => {
  const opened = reduce(base, { type: 'open', id: 'r1' });
  assert.equal(opened.selected, 'r1');
  assert.equal(reduce(opened, { type: 'open', id: null }).selected, 'r1');
});

test('an open item missing from the log stays open so the panel can say so', () => {
  const s = reduce({ ...base, open: 'r9' }, { type: 'items', items: [{ id: 'r1', title: 't', stage: 'ready' }] });
  assert.equal(s.open, 'r9');
});

test('the address round-trips the search and the open item and keeps other parameters', () => {
  const search = toAddress('?room=kbl&gateway=wss%3A%2F%2Fg', { query: 'cache', open: 'r1' });
  assert.deepEqual(fromAddress(search), { view: 'all', query: 'cache', open: 'r1' });
  assert.ok(search.includes('room=kbl'));
  assert.equal(toAddress('?q=x&item=r1', { query: '', open: null }), '');
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
  const search = toAddress('?room=kbl', { view: 'blocked', query: '', open: 'r1' });
  assert.equal(fromAddress(search).view, 'blocked');
  assert.equal(toAddress('?view=blocked', { view: 'all', query: '', open: null }), '');
  assert.equal(fromAddress('?view=board').view, 'all');
});

test('an items update without blocked rows keeps the ones the state has', () => {
  const row = { id: 'b', title: 't', repo: null, source: null, waits_on: [] };
  const s = reduce({ ...base, blocked: [row] }, { type: 'items', items: [] });
  assert.deepEqual(s.blocked, [row]);
  assert.deepEqual(reduce(s, { type: 'items', items: [], blocked: [] }).blocked, []);
  assert.equal(reduce(s, { type: 'view', view: 'blocked' }).view, 'blocked');
});
