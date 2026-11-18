import assert from 'node:assert/strict';
import { test } from 'node:test';
import { reduce, type ViewState } from '../src/state.ts';
import { NO_WRITES } from '../src/writes.ts';
import { collapseKey } from '../src/group-by.ts';
import { flatten, laneEdge, moveLanes, settleSelection, shownRows, swimlanes, visible } from '../src/list.ts';

// Two milestones, three stages. m1: ready [a, b], done [c]. m2: ready [d], triage [e]. none: triage [f].
const items = [
  { id: 'a', title: 'a', stage: 'ready', milestone: 'm1' },
  { id: 'b', title: 'b', stage: 'ready', milestone: 'm1' },
  { id: 'c', title: 'c', stage: 'done', milestone: 'm1' },
  { id: 'd', title: 'd', stage: 'ready', milestone: 'm2' },
  { id: 'e', title: 'e', stage: 'triage', milestone: 'm2' },
  { id: 'f', title: 'f', stage: 'triage' },
];
const base = { items, blocked: [], view: 'all' as const, query: '', filters: {}, layout: 'board' as const, group: 'milestone' };
const initial: ViewState = { connection: { kind: 'setup' }, synced: false, items: [], blocked: [], overlaps: new Map(), stories: new Map(), reviewLoad: { load: 0, inReview: 0 }, view: 'all', query: '', selected: null, open: null, theme: 'dark', layout: 'board', filters: {}, density: 'comfortable', saved: [], marked: [], writes: NO_WRITES, tracker: { kind: 'signed-out' }, questions: new Map(), kind: null, waiting: new Map() };
const fold = (v: string) => ({ ...base, collapsed: [collapseKey('all', 'milestone', v)] });

test('a board grouped by a key becomes lanes of the same stage columns', () => {
  const lanes = swimlanes(base)!;
  assert.deepEqual(lanes.map((l) => l.label), ['m1', 'm2', 'No milestone']);
  for (const l of lanes) assert.deepEqual(l.cells.map((c) => c.column), ['triage', 'ready', 'done'], 'every lane has every column');
  assert.deepEqual(lanes[0].cells.map((c) => c.items.map((i) => i.id).join('')), ['', 'ab', 'c']);
});

test('stage and none keep the plain columns', () => {
  assert.equal(swimlanes({ ...base, group: undefined }), null);
  assert.equal(swimlanes({ ...base, group: 'none' }), null);
  assert.equal(swimlanes({ ...base, layout: 'list' as const }), null);
  assert.deepEqual(visible({ ...base, group: undefined }).map((g) => g.stage), ['triage', 'ready', 'done']);
});

test('visible lists the cells lane by lane, one toggle per lane', () => {
  const g = visible(base);
  assert.equal(g.length, 9);
  assert.deepEqual(g.filter((c) => c.toggle).map((c) => c.toggle), ['all|milestone|m1', 'all|milestone|m2', 'all|milestone|']);
});

test('a folded lane hides its cards from every walk but keeps its header', () => {
  const s = fold('m1');
  assert.equal(swimlanes(s)![0].collapsed, true);
  assert.deepEqual(shownRows(s).map((i) => i.id), ['e', 'd', 'f']);
});

test('up and down walk a column through the open lanes', () => {
  const lanes = swimlanes(base)!;
  assert.equal(moveLanes(lanes, 'a', 'down'), 'b');
  assert.equal(moveLanes(lanes, 'b', 'down'), 'd', 'the last card of a cell steps into the next lane');
  assert.equal(moveLanes(lanes, 'd', 'up'), 'b');
  assert.equal(moveLanes(lanes, 'd', 'down'), 'd', 'the edge stops');
  assert.equal(moveLanes(lanes, 'a', 'up'), 'a');
  assert.equal(moveLanes(swimlanes(fold('m1'))!, 'e', 'down'), 'f', 'a folded lane is skipped');
  assert.equal(moveLanes(swimlanes(fold('m1'))!, 'd', 'up'), 'd', 'and never landed on');
});

test('left and right stay in the lane and skip empty columns', () => {
  const lanes = swimlanes(base)!;
  assert.equal(moveLanes(lanes, 'a', 'right'), 'c');
  assert.equal(moveLanes(lanes, 'b', 'right'), 'c', 'a shorter cell lands on its last card');
  assert.equal(moveLanes(lanes, 'c', 'left'), 'a');
  assert.equal(moveLanes(lanes, 'a', 'left'), 'a', 'm1 has nothing in triage');
  assert.equal(moveLanes(lanes, 'e', 'right'), 'd');
  assert.equal(moveLanes(lanes, 'd', 'right'), 'd');
});

test('no usable current card starts at the first drawn card', () => {
  assert.equal(moveLanes(swimlanes(base)!, null, 'down'), 'a');
  assert.equal(moveLanes(swimlanes(fold('m1'))!, 'a', 'down'), 'e', 'a card in a folded lane is not current');
  assert.equal(moveLanes([], null, 'down'), null);
});

test('Home and End stay in the column across open lanes', () => {
  const lanes = swimlanes(base)!;
  assert.equal(laneEdge(lanes, 'd', 'first'), 'a');
  assert.equal(laneEdge(lanes, 'a', 'last'), 'd');
  assert.equal(laneEdge(lanes, 'e', 'first'), 'e');
  assert.equal(laneEdge(swimlanes(fold('m2'))!, 'a', 'last'), 'b');
  assert.equal(laneEdge([], null, 'first'), null);
});

test('folding the lane under the selection moves it to a drawn card', () => {
  const s = { ...fold('m1'), selected: 'b' };
  assert.equal(settleSelection(s), 'e');
});

test('a live update that regroups an item moves it between lanes', () => {
  const moved = items.map((i) => (i.id === 'a' ? { ...i, milestone: 'm2' } : i));
  const lanes = swimlanes({ ...base, items: moved })!;
  assert.deepEqual(lanes[1].cells.find((c) => c.column === 'ready')!.items.map((i) => i.id), ['a', 'd']);
  assert.equal(moveLanes(lanes, 'b', 'down'), 'a');
});

test('an update that lands the selection in a folded lane settles it', () => {
  const s = { ...initial, ...fold('m2'), items, selected: 'a' };
  const next = reduce(s, { type: 'items', items: items.map((i) => (i.id === 'a' ? { ...i, milestone: 'm2' } : i)), blocked: [], overlaps: new Map() });
  assert.equal(next.selected, 'f', 'the nearest card that is still drawn');
});
