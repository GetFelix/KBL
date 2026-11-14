import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { ItemRow } from '../src/fold.ts';
import {
  activeCount,
  applyFilters,
  decodeView,
  deleteView,
  encodeView,
  facets,
  parseSaved,
  renameView,
  sameView,
  saveView,
  setFilter,
  summary,
} from '../src/views.ts';

const items: ItemRow[] = [
  { id: 'a', title: 'one', stage: 'ready', tier: 1, source: 'github', repo: 'GetFelix/felix' },
  { id: 'b', title: 'two', stage: 'ready', tier: 2, source: 'github', repo: 'GetFelix/KBL' },
  { id: 'c', title: 'three', stage: 'triage', tier: null, source: 'jira', repo: null },
  { id: 'd', title: 'four', stage: 'triage' },
];

test('facets offer only recorded values, with counts, and leave out facets nobody recorded', () => {
  const f = facets(items);
  assert.deepEqual(f.map((x) => x.facet), ['source', 'tier', 'repo']);
  assert.deepEqual(f[0].values, [['github', 2], ['jira', 1]]);
  assert.deepEqual(f[1].values, [['1', 1], ['2', 1]]);
  assert.deepEqual(facets([items[3]]), []);
});

test('filters match recorded values only, and combine', () => {
  assert.deepEqual(applyFilters(items, {}).map((i) => i.id), ['a', 'b', 'c', 'd']);
  assert.deepEqual(applyFilters(items, { source: 'github' }).map((i) => i.id), ['a', 'b']);
  assert.deepEqual(applyFilters(items, { source: 'github', tier: '2' }).map((i) => i.id), ['b']);
  assert.deepEqual(applyFilters(items, { repo: 'GetFelix/KBL', tier: '1' }), [], 'an item without a value never matches it');
  const f = setFilter(setFilter({}, 'tier', '1'), 'source', 'github');
  assert.equal(summary(f), 'Source github, Tier 1');
  assert.equal(activeCount(f), 2);
  assert.deepEqual(setFilter(f, 'tier', null), { source: 'github' });
});

test('the view round-trips through the address and keeps other parameters', () => {
  const v = { layout: 'board' as const, query: 'lease', filters: { repo: 'GetFelix/felix', tier: '1' } };
  const search = encodeView('room=kbl&gateway=wss%3A%2F%2Fg&item=a', v);
  assert.deepEqual(decodeView(search), v);
  assert.ok(search.includes('room=kbl') && search.includes('item=a'));
  assert.equal(encodeView('view=board&q=x&tier=1', { layout: 'list', query: '', filters: {} }), '', 'defaults leave nothing behind');
  assert.equal(decodeView('view=grid').layout, 'list');
  assert.ok(sameView(decodeView('tier=1&q=x'), decodeView('q=x&tier=1')));
});

test('saved views add, replace by name, rename without clobbering, and delete', () => {
  const v = { layout: 'list' as const, query: '', filters: { source: 'github' } };
  let list = saveView([], ' GitHub ', v);
  assert.deepEqual(list, [{ name: 'GitHub', search: 'source=github' }]);
  list = saveView(list, 'Tier 1', { ...v, filters: { tier: '1' } });
  list = saveView(list, 'github', { ...v, layout: 'board' });
  assert.deepEqual(list.map((x) => [x.name, x.search]), [['github', 'view=board&source=github'], ['Tier 1', 'tier=1']]);
  assert.equal(renameView(list, 'Tier 1', 'GITHUB'), null, 'a name already taken is refused');
  assert.equal(renameView(list, 'Tier 1', '  '), null);
  assert.deepEqual(renameView(list, 'Tier 1', 'Urgent')?.map((x) => x.name), ['github', 'Urgent']);
  assert.deepEqual(renameView(list, 'github', 'GitHub')?.map((x) => x.name), ['GitHub', 'Tier 1'], 'a view may change its own case');
  assert.deepEqual(deleteView(list, 'github').map((x) => x.name), ['Tier 1']);
});

test('saved views from storage drop anything malformed', () => {
  assert.deepEqual(parseSaved(null), []);
  assert.deepEqual(parseSaved('{'), []);
  assert.deepEqual(parseSaved('{"name":"x"}'), []);
  assert.deepEqual(parseSaved('[{"name":"ok","search":"tier=1"},{"name":""},{"name":"n","search":3},null]'), [{ name: 'ok', search: 'tier=1' }]);
});
