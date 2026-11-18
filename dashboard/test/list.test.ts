import assert from 'node:assert/strict';
import { test } from 'node:test';
import { boardEdge, flatten, group, inScope, move, moveBoard, neighbour, shownRows, stageLabel, visible } from '../src/list.ts';

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
  const s = { items: tiered, blocked: [], view: 'all' as const };
  assert.deepEqual(flatten(visible({ ...s, query: '', filters: { tier: '1' } })).map((i) => i.id), ['b', 'd']);
  assert.deepEqual(flatten(visible({ ...s, query: 'which', filters: { tier: '1' } })).map((i) => i.id), ['b']);
  assert.deepEqual(visible({ ...s, query: '', filters: { source: 'jira' } }), []);
});

test('Home and End on the board stay in the column', () => {
  const g = group(rows); // needs_you: b e, triage: c, done: a, parked: d
  assert.equal(boardEdge(g, 'e', 'first'), 'b');
  assert.equal(boardEdge(g, 'b', 'last'), 'e');
  assert.equal(boardEdge(g, 'c', 'last'), 'c');
  assert.equal(boardEdge(g, null, 'last'), 'e', 'no selection uses the first column');
  assert.equal(boardEdge([], null, 'first'), null);
});

test('a focused item that leaves hands the focus to the next one, else the previous one', () => {
  const before = ['a', 'b', 'c', 'd'];
  assert.equal(neighbour(before, 'b', (id) => id !== 'b'), 'c');
  assert.equal(neighbour(before, 'b', (id) => id === 'a' || id === 'd'), 'd', 'skips ones that left too');
  assert.equal(neighbour(before, 'd', (id) => id !== 'd'), 'c', 'the last one falls back to the previous');
  assert.equal(neighbour(before, 'b', () => false), undefined);
  assert.equal(neighbour(before, 'x', () => true), undefined);
});

test('the Blocked view lists only blocked items, and j and k walk only those', () => {
  const items = [
    { id: 'a', title: 'one', stage: 'ready' },
    { id: 'b', title: 'two', stage: 'blocked' },
    { id: 'c', title: 'three', stage: 'blocked' },
  ];
  const blocked = ['b', 'c'].map((id) => ({ id, title: id, repo: null, source: null, waits_on: [] }));
  assert.equal(inScope({ items, blocked, view: 'all' }).length, 3);
  assert.deepEqual(inScope({ items, blocked, view: 'blocked' }).map((i) => i.id), ['b', 'c']);
  assert.deepEqual(shownRows({ items, blocked, view: 'blocked', query: 'thr', filters: {} }).map((i) => i.id), ['c']);
});

test('the Blocked view composes with filters', () => {
  const items = [
    { id: 'a', title: 'one', stage: 'blocked', tier: 1 },
    { id: 'b', title: 'two', stage: 'blocked', tier: 2 },
    { id: 'c', title: 'three', stage: 'ready', tier: 1 },
  ];
  const blocked = ['a', 'b'].map((id) => ({ id, title: id, repo: null, source: null, waits_on: [] }));
  assert.deepEqual(shownRows({ items, blocked, view: 'blocked', query: '', filters: { tier: '1' } }).map((i) => i.id), ['a']);
  assert.deepEqual(shownRows({ items, blocked, view: 'all', query: '', filters: { tier: '1' } }).map((i) => i.id).sort(), ['a', 'c']);
});

test('a selected id the view no longer shows starts j and k from the ends', () => {
  const items = [
    { id: 'a', title: 'one', stage: 'blocked' },
    { id: 'b', title: 'two', stage: 'blocked' },
  ];
  const state = { items, blocked: [{ id: 'a', title: 'a', repo: null, source: null, waits_on: [] }], view: 'blocked' as const, query: '', filters: {} };
  const shown = shownRows(state);
  assert.deepEqual(shown.map((i) => i.id), ['a']);
  assert.equal(move(shown, 'b', 1), 'a');
  assert.equal(move(shown, 'b', -1), 'a');
});

test('child work nests under its root unless it needs a person or the toggle shows cards', () => {
  const items = [
    { id: 'r', title: 'root', stage: 'ready', root: true },
    { id: 'r-t', title: 'triage', stage: 'done', parent: 'r' },
    { id: 'r-q', title: 'asks', stage: 'needs_you', parent: 'r' },
    { id: 'orphan', title: 'child of a hidden parent', stage: 'ready', parent: 'gone' },
  ];
  const base = { items, blocked: [], view: 'all' as const, query: '', filters: {} };
  assert.deepEqual(shownRows(base).map((i) => i.id).sort(), ['orphan', 'r', 'r-q']);
  assert.deepEqual(shownRows({ ...base, childCards: true }).map((i) => i.id).sort(), ['orphan', 'r', 'r-q', 'r-t']);
  // A search that matches only the child shows it, since its root is not shown.
  assert.deepEqual(shownRows({ ...base, query: 'triage' }).map((i) => i.id), ['r-t']);
});
