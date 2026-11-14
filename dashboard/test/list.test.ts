import assert from 'node:assert/strict';
import { test } from 'node:test';
import { flatten, group, move, stageLabel } from '../src/list.ts';

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
