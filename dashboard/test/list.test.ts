import assert from 'node:assert/strict';
import { test } from 'node:test';
import { flatten, group, move, moveBoard, stageLabel, visible } from '../src/list.ts';

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

test('the board moves within a column and across columns, keeping the row', () => {
  const cols = group(rows);
  // needs_you [b, e] | triage [c] | done [a] | parked [d]
  assert.equal(moveBoard(cols, null, 'down'), 'b');
  assert.equal(moveBoard(cols, 'b', 'down'), 'e');
  assert.equal(moveBoard(cols, 'e', 'down'), 'e');
  assert.equal(moveBoard(cols, 'e', 'up'), 'b');
  assert.equal(moveBoard(cols, 'e', 'right'), 'c', 'a shorter column lands on its last card');
  assert.equal(moveBoard(cols, 'c', 'left'), 'b', 'row 0 stays row 0');
  assert.equal(moveBoard(cols, 'b', 'left'), 'b');
  assert.equal(moveBoard(cols, 'd', 'right'), 'd');
  assert.equal(moveBoard(cols, 'gone', 'left'), 'b', 'a selection that is no longer shown starts over');
  assert.equal(moveBoard([], null, 'down'), null);
});

test('visible applies the filters before the search', () => {
  const tiered = rows.map((r, i) => ({ ...r, tier: i % 2 ? 1 : 2, source: 'github' }));
  assert.deepEqual(flatten(visible(tiered, '', { tier: '1' })).map((i) => i.id), ['b', 'd']);
  assert.deepEqual(flatten(visible(tiered, 'which', { tier: '1' })).map((i) => i.id), ['b']);
  assert.deepEqual(visible(tiered, '', { source: 'jira' }), []);
});
