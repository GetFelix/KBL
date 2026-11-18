import assert from 'node:assert/strict';
import test from 'node:test';
import type { ItemRow } from '../src/fold.ts';
import { inboxFor, inboxKind, kindCounts, optionForKey, oldestFirst, trackWaiting } from '../src/inbox.ts';
import { visible } from '../src/list.ts';

const item = (o: Partial<ItemRow> & { id: string }): ItemRow => ({ title: o.id, stage: 'ready', tier: 1, format: 14, ...o });

const rows = [
  item({ id: 'q', stage: 'needs_you', needs: 'question', question: 40 }),
  item({ id: 'u', stage: 'needs_you', needs: 'unclear', question: 12 }),
  item({ id: 'p', stage: 'triage', tier: null, proposed_tier: 2 }),
  item({ id: 'r', stage: 'proposed', tier: null, finding: 7 }),
  item({ id: 'd', stage: 'needs_you', needs: 'dependency_dropped' }),
  item({ id: 'new', stage: 'triage', tier: null }),
  item({ id: 'ready' }),
];

test('each item waiting on a person has a kind, and the rest have none', () => {
  assert.deepEqual(rows.map((i) => inboxKind(i)), ['question', 'question', 'triage', 'review', 'decision', null, null]);
  assert.deepEqual(kindCounts(rows), { question: 2, triage: 1, review: 1, decision: 1 });
});

test('the inbox keeps what waits on the viewer and can be narrowed to one kind', () => {
  assert.deepEqual(inboxFor(rows, null).map((i) => i.id), ['q', 'u', 'p', 'r', 'd']);
  assert.deepEqual(inboxFor(rows, null, 'triage').map((i) => i.id), ['p']);
  const hers = [...rows, item({ id: 'h', stage: 'needs_you', needs: 'taken', assignee: { person: 'ana' } })];
  assert.equal(inboxFor(hers, 'bo').some((i) => i.id === 'h'), false);
  assert.equal(inboxFor(hers, 'ana').some((i) => i.id === 'h'), true);
});

test('the oldest wait comes first: a question by its offset, the rest by when they began', () => {
  const prev = trackWaiting(new Map(), rows, 100);
  assert.deepEqual(oldestFirst(inboxFor(rows, null), prev).map((i) => i.id), ['u', 'q', 'd', 'p', 'r']);
  // The reason is unchanged, so a later look keeps the first offset; a new reason starts over.
  const later = trackWaiting(prev, [...rows.filter((i) => i.id !== 'd'), item({ id: 'd', stage: 'needs_you', needs: 'check_failed' })], 200);
  assert.equal(later.get('p')!.at, 100);
  assert.equal(later.get('d')!.at, 200);
  assert.deepEqual(oldestFirst(inboxFor(rows, null), later).map((i) => i.id).slice(0, 2), ['u', 'q']);
});

test('the fold\u2019s waiting offset orders the inbox, whatever the page saw first', () => {
  const seen = trackWaiting(new Map(), rows, 100);
  const folded = [
    item({ id: 'b', stage: 'triage', tier: null, proposed_tier: 1, waiting_at: 30 }),
    item({ id: 'a', stage: 'triage', tier: null, proposed_tier: 1, waiting_at: 30 }),
    item({ id: 'c', stage: 'needs_you', needs: 'check_failed', waiting_at: 5 }),
    item({ id: 'q2', stage: 'needs_you', needs: 'question', question: 20, waiting_at: 20 }),
    // A fold that did not record it falls back to the page's own count, after every recorded wait.
    item({ id: 'old', stage: 'needs_you', needs: 'check_failed' }),
  ];
  assert.deepEqual(oldestFirst(folded, seen).map((i) => i.id), ['c', 'q2', 'a', 'b', 'old']);
  // The same rows given in another order and with a different page history give the same list.
  assert.deepEqual(oldestFirst([...folded].reverse(), new Map()).map((i) => i.id), ['c', 'q2', 'a', 'b', 'old']);
});

test('the inbox view is one list in that order, with the kind filter applied', () => {
  const s = { items: rows, blocked: [], view: 'inbox' as const, query: '', filters: {}, kind: null, waiting: trackWaiting(new Map(), rows, 100) };
  assert.deepEqual(visible(s).map((g) => [g.stage, g.items.map((i) => i.id).join('')]), [['inbox', 'uqdpr']]);
  assert.deepEqual(visible({ ...s, kind: 'question' }).flatMap((g) => g.items.map((i) => i.id)), ['u', 'q']);
  assert.deepEqual(visible({ ...s, kind: 'review' }).flatMap((g) => g.items.map((i) => i.id)), ['r']);
});

test('a digit picks the numbered option', () => {
  assert.equal(optionForKey(['a', 'b'], '2'), 'b');
  assert.equal(optionForKey(['a', 'b'], '3'), null);
  assert.equal(optionForKey(['a'], '0'), null);
});
