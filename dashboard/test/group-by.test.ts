import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { ItemRow } from '../src/fold.ts';
import { collapseKey, defaultGroup, effectiveGroup, GROUP_KEYS, groupItems } from '../src/group-by.ts';
import { flatten, move, settleSelection, shownRows, visible } from '../src/list.ts';
import { createStore, fromAddress, reduce, toAddress, type ViewState } from '../src/state.ts';
import { decodeView, encodeView, parseSaved, sameView, saveView } from '../src/views.ts';
import { NO_WRITES } from '../src/writes.ts';

const item = (id: string, more: Partial<ItemRow> = {}): ItemRow => ({ id, title: id, stage: 'ready', ...more }) as ItemRow;
const items = [
  item('a', { tier: 2, repo: 'x/one', work: 'fix', assignee: { person: 'ana' } }),
  item('b', { tier: 1, repo: 'x/two', work: 'fix', stage: 'needs_you' }),
  item('c', { tier: 1, repo: 'x/one', assignee: { agent: 'bot' } }),
  item('d', { tier: 10, repo: null, work: 'review', stage: 'triage' }),
  item('e', { tier: 2, repo: 'x/one', work: 'fix', assignee: { person: 'ana' } }),
];
const base: ViewState = { connection: { kind: 'setup' }, synced: false, items, blocked: [], overlaps: new Map(), stories: new Map(), reviewLoad: { load: 0, inReview: 0 }, view: 'all', query: '', selected: null, open: null, theme: 'dark', layout: 'list', filters: {}, density: 'comfortable', saved: [], marked: [], writes: NO_WRITES, tracker: { kind: 'signed-out' }, questions: new Map(), kind: null, waiting: new Map() };
const ids = (s: Partial<ViewState>) => visible({ ...base, ...s }).map((g) => [g.label, g.items.map((i) => i.id).join('')]);

test('every key groups by its field, in its own order, and items with no value group last', () => {
  assert.deepEqual(ids({ group: 'tier' }), [['Tier 1', 'bc'], ['Tier 2', 'ae'], ['Tier 10', 'd']]);
  assert.deepEqual(ids({ group: 'repo' }), [['x/one', 'ace'], ['x/two', 'b'], ['No repo', 'd']]);
  assert.deepEqual(ids({ group: 'kind' }), [['Fix', 'abe'], ['Review', 'd'], ['No kind', 'c']]);
  assert.deepEqual(ids({ group: 'assignee' }), [['ana', 'ae'], ['Agent bot', 'c'], ['Unassigned', 'bd']]);
  assert.deepEqual(ids({ group: 'stage' }).map(([l]) => l), ['Needs you', 'Triage', 'Ready']);
  assert.deepEqual(ids({}), ids({ group: 'stage' }));
});

test('none is one list with no heading, in the item order', () => {
  const g = visible({ ...base, group: 'none' });
  assert.deepEqual(g.map((x) => [x.label, x.toggle]), [['', undefined]]);
  assert.deepEqual(flatten(g).map((i) => i.id), ['a', 'b', 'c', 'd', 'e']);
});

test('the board keeps stage columns when grouped, and none or stage add no lanes', () => {
  assert.deepEqual(new Set(visible({ ...base, layout: 'board', group: 'tier' }).map((g) => g.column)), new Set(['needs_you', 'triage', 'ready']));
  assert.deepEqual(visible({ ...base, layout: 'board', group: 'stage' }).map((g) => g.stage), ['needs_you', 'triage', 'ready']);
  assert.equal(visible({ ...base, layout: 'board', group: 'none' }).length, 3);
});

test('the inbox stays one oldest-first list unless grouped, and groups keep that order inside', () => {
  const waiting = new Map([['b', { at: 5 }], ['c', { at: 1 }]]) as unknown as ViewState['waiting'];
  const inbox = { ...base, view: 'inbox' as const, waiting, items: [item('b', { stage: 'needs_you', needs: 'question', tier: 1 }), item('c', { stage: 'needs_you', needs: 'question', tier: 1 })] };
  assert.deepEqual(visible(inbox).map((g) => g.stage), ['inbox']);
  const grouped = visible({ ...inbox, group: 'tier' });
  assert.deepEqual(grouped.map((g) => g.label), ['Tier 1']);
  assert.equal(defaultGroup('inbox'), 'none');
  assert.equal(effectiveGroup('all', 'nonsense'), 'stage');
});

test('j and k skip a collapsed group and walk only drawn rows', () => {
  const folded = [collapseKey('all', 'tier', '2')];
  const s = { ...base, group: 'tier', collapsed: folded };
  assert.deepEqual(shownRows(s).map((i) => i.id), ['b', 'c', 'd']);
  assert.equal(move(shownRows(s), 'c', 1), 'd');
  // The group is still listed, with its count, so the header can open it.
  assert.deepEqual(visible(s).map((g) => [g.label, g.items.length, !!g.collapsed]), [['Tier 1', 2, false], ['Tier 2', 2, true], ['Tier 10', 1, false]]);
  // A collapse is per view and per grouping.
  assert.equal(shownRows({ ...s, view: 'blocked' as const, blocked: items.map((i) => ({ id: i.id }) as never) }).length, 5);
  assert.equal(shownRows({ ...s, group: 'repo' }).length, 5);
});

test('folding the group the selection is in moves it to the nearest drawn row', () => {
  const picked = { ...base, group: 'tier', selected: 'a' };
  const folded = reduce(picked, { type: 'collapse', key: collapseKey('all', 'tier', '2') });
  assert.equal(folded.selected, 'd');
  const last = reduce({ ...picked, items: items.slice(0, 1), selected: 'a' }, { type: 'collapse', key: collapseKey('all', 'tier', '2') });
  assert.equal(last.selected, null);
  assert.equal(reduce(folded, { type: 'collapse', key: collapseKey('all', 'tier', '2') }).collapsed?.length, 0);
});

test('a live update that lands the selected item in a folded group moves the selection', () => {
  const s = { ...base, group: 'tier', collapsed: [collapseKey('all', 'tier', '1')], selected: 'a' };
  const moved = reduce(s, { type: 'items', items: items.map((i) => (i.id === 'a' ? { ...i, tier: 1 } : i)), blocked: [], overlaps: new Map() });
  assert.notEqual(moved.selected, 'a');
  assert.ok(shownRows(moved).some((i) => i.id === moved.selected));
  // A selection the view never listed is left alone.
  assert.equal(settleSelection({ ...s, selected: 'nope' }), 'nope');
});

test('regrouping into a folded group settles the selection too, and expand all opens every group', () => {
  const s = { ...base, collapsed: [collapseKey('all', 'tier', '2')], selected: 'a' };
  assert.equal(reduce(s, { type: 'group', group: 'tier' }).selected, 'd');
  const all = reduce({ ...base, group: 'tier' }, { type: 'collapseAll', collapsed: true });
  assert.deepEqual(shownRows(all), []);
  assert.equal(shownRows(reduce(all, { type: 'collapseAll', collapsed: false })).length, 5);
});

test('collapse all keeps what other views and groupings folded', () => {
  const other = collapseKey('blocked', 'stage', 'ready');
  const all = reduce({ ...base, group: 'tier', collapsed: [other] }, { type: 'collapseAll', collapsed: true });
  assert.ok(all.collapsed?.includes(other));
  assert.deepEqual(reduce(all, { type: 'collapseAll', collapsed: false }).collapsed, [other]);
});

test('a group is in the address and in a saved view, and the default is left out', () => {
  assert.equal(encodeView('', { view: 'all', layout: 'list', query: '', filters: {}, group: 'tier' }), 'group=tier');
  assert.equal(encodeView('', { view: 'all', layout: 'list', query: '', filters: {}, group: 'stage' }), '');
  assert.equal(encodeView('', { view: 'inbox', layout: 'list', query: '', filters: {}, group: 'none' }), 'view=inbox');
  assert.equal(encodeView('', { view: 'all', layout: 'list', query: '', filters: {}, group: 'none' }), 'group=none');
  assert.deepEqual(decodeView('group=repo'), { view: 'all', layout: 'list', query: '', filters: {}, group: 'repo' });
  assert.equal('group' in decodeView(''), false);
  assert.equal('group' in decodeView('group=nonsense'), false);
  const saved = saveView([], 'By tier', { view: 'all', layout: 'list', query: '', filters: { repo: 'x/one' }, group: 'tier' });
  assert.ok(Array.isArray(saved));
  const back = parseSaved(JSON.stringify(saved));
  assert.deepEqual(decodeView(back[0].search).group, 'tier');
  assert.equal(sameView(decodeView('group=tier'), decodeView('group=tier&layout=list')), true);
  assert.equal(sameView(decodeView('group=tier'), decodeView('')), false);
  const addr = toAddress('?room=r', { view: 'all', query: '', open: null, layout: 'list', filters: {}, group: 'assignee' });
  assert.equal(addr, '?room=r&group=assignee');
  assert.equal(fromAddress(addr).group, 'assignee');
});

test('opening a saved view replaces the grouping, back to the default when it has none', () => {
  const grouped = reduce({ ...base, group: 'repo' }, { type: 'openView', search: 'group=tier' });
  assert.equal(grouped.group, 'tier');
  assert.equal(reduce({ ...base, group: 'repo' }, { type: 'openView', search: 'layout=board' }).group, undefined);
  assert.equal(createStore(base).get().group, undefined);
});

test('part B adds a key with one table entry: the table carries everything a key needs', () => {
  for (const k of GROUP_KEYS) {
    assert.ok(k.id && k.label && k.none, k.id);
    for (const i of items) assert.ok(k.value(i) === null || typeof k.value(i) === 'string', k.id);
  }
  assert.deepEqual(groupItems(items, 'nope'), []);
});

test('milestone, label, type, area and size group by what the log recorded', () => {
  const rows = groupItems(
    [
      item('a', { milestone: '0.9', labels: ['perf', 'Docs'], class: { item_type: { value: 'bug', by: 'triage' }, size: { value: 'L', by: 'person' } } }),
      item('b', { milestone: '0.10', labels: ['perf'], class: { item_type: { value: 'bug', by: 'rule' }, area: { value: 'storage', by: 'triage' }, size: { value: 'S', by: 'triage' } } }),
      item('c'),
    ],
    'milestone',
  );
  assert.deepEqual(rows.map((g) => [g.label, g.items.map((i) => i.id).join('')]), [['0.9', 'a'], ['0.10', 'b'], ['No milestone', 'c']]);
  const by = (key: string) => groupItems([item('a', { labels: ['perf', 'Docs'], class: { item_type: { value: 'bug', by: 'person' }, size: { value: 'L', by: 'person' } } }), item('b', { class: { size: { value: 'S', by: 'rule' }, area: { value: 'storage', by: 'rule' } } }), item('c')], key).map((g) => [g.label, g.items.map((i) => i.id).join('')]);
  // An item with several labels sits under its first one alphabetically.
  assert.deepEqual(by('label'), [['Docs', 'a'], ['No label', 'bc']]);
  assert.deepEqual(by('type'), [['bug', 'a'], ['No type', 'bc']]);
  assert.deepEqual(by('area'), [['storage', 'b'], ['No area', 'ac']]);
  // Sizes run small to large, not alphabetically.
  assert.deepEqual(by('size'), [['S', 'b'], ['L', 'a'], ['No size', 'c']]);
});

test('a layout, view or saved-view change moves a selection off a folded row', () => {
  const s = { ...base, group: 'tier', collapsed: [collapseKey('all', 'tier', '2')], layout: 'board' as const, selected: 'a' };
  assert.equal(reduce(s, { type: 'layout', layout: 'list' }).selected, 'd');
  assert.notEqual(reduce({ ...s, layout: 'list' as const, selected: 'b' }, { type: 'openView', search: 'group=tier' }).selected, null);
});

test('picking the view default is no choice, so the inbox keeps its own default', () => {
  const s = reduce(base, { type: 'group', group: 'stage' });
  assert.equal(s.group, undefined);
  assert.equal(reduce(base, { type: 'group', group: 'tier' }).group, 'tier');
});
