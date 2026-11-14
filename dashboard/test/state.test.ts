import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createStore, describe, fromAddress, reduce, toAddress, type ViewState } from '../src/state.ts';

const base: ViewState = { connection: { kind: 'setup' }, items: [], query: '', selected: null, open: null, theme: 'dark' };

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
  assert.deepEqual(fromAddress(search), { query: 'cache', open: 'r1' });
  assert.ok(search.includes('room=kbl'));
  assert.equal(toAddress('?q=x&item=r1', { query: '', open: null }), '');
});

test('the status line says what the page is doing and what failed', () => {
  assert.deepEqual(describe({ kind: 'live', refused: 0 }, 1), { text: 'Live, 1 item', tone: 'ok' });
  assert.equal(describe({ kind: 'live', refused: 2 }, 3).tone, 'warn');
  assert.match(describe({ kind: 'retrying', reason: 'closed', seconds: 4 }, 0).text, /Retrying in 4 s/);
  assert.equal(describe({ kind: 'stopped', reason: 'signed_out: expired' }, 0).tone, 'error');
});

test('the store hands each new state to its subscribers', () => {
  const store = createStore(base);
  let calls = 0;
  store.subscribe(() => calls++);
  store.dispatch({ type: 'query', query: 'x' });
  assert.equal(calls, 1);
  assert.equal(store.get().query, 'x');
});
