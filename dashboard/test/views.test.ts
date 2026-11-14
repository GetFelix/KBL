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
  type SavedView,
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
  const v = { view: 'blocked' as const, layout: 'board' as const, query: 'lease', filters: { repo: 'GetFelix/felix', tier: '1' } };
  const search = encodeView('room=kbl&gateway=wss%3A%2F%2Fg&item=a', v);
  assert.deepEqual(decodeView(search), v);
  assert.ok(search.includes('room=kbl') && search.includes('item=a'));
  assert.equal(encodeView('view=blocked&layout=board&q=x&tier=1', { view: 'all', layout: 'list', query: '', filters: {} }), '', 'defaults leave nothing behind');
  assert.equal(decodeView('layout=grid').layout, 'list');
  assert.equal(decodeView('view=grid').view, 'all');
  assert.ok(sameView(decodeView('tier=1&q=x'), decodeView('q=x&tier=1')));
});

test('saved views add, refuse a taken name, rename without clobbering, and delete', () => {
  const v = { view: 'all' as const, layout: 'list' as const, query: '', filters: { source: 'github' } };
  let list = saveView([], ' GitHub ', v) as SavedView[];
  assert.deepEqual(list, [{ name: 'GitHub', search: 'source=github' }]);
  list = saveView(list, 'Tier 1', { ...v, filters: { tier: '1' } }) as SavedView[];
  assert.equal(saveView(list, 'github', { ...v, layout: 'board' }), 'A view is already called “github”. Pick another name, or edit that view.', 'saving under a taken name is refused');
  assert.equal(saveView(list, '  ', v), 'Give the view a name.');
  assert.match(renameView(list, 'Tier 1', 'GITHUB') as string, /already called/, 'a name already taken is refused');
  assert.equal(renameView(list, 'Tier 1', '  '), 'Give the view a name.');
  assert.match(renameView(list, 'Gone', 'Other') as string, /deleted/, 'a view another tab deleted is not renamed');
  assert.deepEqual((renameView(list, 'Tier 1', 'Urgent') as SavedView[]).map((x) => x.name), ['GitHub', 'Urgent']);
  assert.deepEqual((renameView(list, 'GitHub', 'github') as SavedView[]).map((x) => x.name), ['github', 'Tier 1'], 'a view may change its own case');
  assert.deepEqual(deleteView(list, 'GitHub').map((x) => x.name), ['Tier 1']);
});

test('option counts follow the other active filters, and every recorded value stays offered', () => {
  const f = facets(items, { repo: 'GetFelix/KBL' });
  assert.deepEqual(f[0].values, [['github', 1], ['jira', 0]], 'source counts inside the repo filter');
  assert.deepEqual(f[2].values, [['GetFelix/felix', 1], ['GetFelix/KBL', 1]], 'a facet ignores its own filter');
});

test('saved views from storage drop anything malformed', () => {
  assert.deepEqual(parseSaved(null), []);
  assert.deepEqual(parseSaved('{'), []);
  assert.deepEqual(parseSaved('{"name":"x"}'), []);
  assert.deepEqual(parseSaved('[{"name":"ok","search":"tier=1"},{"name":""},{"name":"n","search":3},null]'), [{ name: 'ok', search: 'tier=1' }]);
});

test('a saved view from before the layout parameter opens the board and is read back in the new form', () => {
  const [old] = parseSaved('[{"name":"Board","search":"view=board&tier=1"}]');
  assert.deepEqual(old, { name: 'Board', search: 'layout=board&tier=1' });
  assert.deepEqual(decodeView('view=board&tier=1'), { view: 'all', layout: 'board', query: '', filters: { tier: '1' } });
  assert.ok(sameView(decodeView('view=board'), decodeView('layout=board')));
  assert.equal(decodeView('view=blocked&layout=board').view, 'blocked');
});

test('a saved view keeps parameters this build does not know, so an older tab cannot strip them', () => {
  const [v] = parseSaved('[{"name":"Legacy","search":"view=board&foo=1&q=x"}]');
  assert.deepEqual(new URLSearchParams(v.search).getAll('foo'), ['1']);
  assert.deepEqual(decodeView(v.search), { view: 'all', layout: 'board', query: 'x', filters: {} });
  assert.equal(v.search, 'foo=1&q=x&layout=board');
  assert.equal(parseSaved(JSON.stringify([v]))[0].search, v.search);
});
