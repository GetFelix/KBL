import assert from 'node:assert/strict';
import { test } from 'node:test';
import { flatten, group, move, shownRows, stageLabel, visible } from '../src/list.ts';

const rows = [
  { id: 'a', title: 'ship the release', stage: 'done' },
  { id: 'b', title: 'which client?', stage: 'needs_you' },
  { id: 'c', title: 'reads miss a write', stage: 'triage' },
  { id: 'd', title: 'new kind', stage: 'parked' },
  { id: 'e', title: 'answer it', stage: 'needs_you' },
];

test('groups follow board order, unknown stages last, empty groups left out', () => {
  const g = group(rows);
  assert.deepEqual(g.map((x) => x.stage), ['needs_you', 'triage', 'done', 'parked']);
  assert.deepEqual(g[0].items.map((i) => i.id), ['b', 'e']);
  assert.equal(g[3].label, 'Parked');
  assert.equal(stageLabel('in_review'), 'In review');
});

test('search matches ids and titles', () => {
  assert.deepEqual(flatten(group(rows, 'client')).map((i) => i.id), ['b']);
  assert.deepEqual(flatten(group(rows, 'd')).map((i) => i.id).includes('d'), true);
  assert.deepEqual(group(rows, 'zzz'), []);
});

test('j and k walk display order and stop at the ends', () => {
  const order = flatten(group(rows));
  assert.deepEqual(order.map((i) => i.id), ['b', 'e', 'c', 'a', 'd']);
  assert.equal(move(order, null, 1), 'b');
  assert.equal(move(order, null, -1), 'd');
  assert.equal(move(order, 'b', 1), 'e');
  assert.equal(move(order, 'b', -1), 'b');
  assert.equal(move(order, 'd', 1), 'd');
  assert.equal(move([], 'b', 1), null);
});

test('the Blocked view lists only blocked items, and j and k walk only those', () => {
  const items = [
    { id: 'a', title: 'one', stage: 'ready' },
    { id: 'b', title: 'two', stage: 'blocked' },
    { id: 'c', title: 'three', stage: 'blocked' },
  ];
  const blocked = ['b', 'c'].map((id) => ({ id, title: id, repo: null, source: null, waits_on: [] }));
  assert.equal(visible({ items, blocked, view: 'all' }).length, 3);
  assert.deepEqual(visible({ items, blocked, view: 'blocked' }).map((i) => i.id), ['b', 'c']);
  assert.deepEqual(shownRows({ items, blocked, view: 'blocked', query: 'thr' }).map((i) => i.id), ['c']);
});

test('a selected id the view no longer shows starts j and k from the ends', () => {
  const items = [
    { id: 'a', title: 'one', stage: 'blocked' },
    { id: 'b', title: 'two', stage: 'blocked' },
  ];
  const state = { items, blocked: [{ id: 'a', title: 'a', repo: null, source: null, waits_on: [] }], view: 'blocked' as const, query: '' };
  const shown = shownRows(state);
  assert.deepEqual(shown.map((i) => i.id), ['a']);
  assert.equal(move(shown, 'b', 1), 'a');
  assert.equal(move(shown, 'b', -1), 'a');
});
