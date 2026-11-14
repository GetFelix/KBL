import assert from 'node:assert/strict';
import { test } from 'node:test';
import { rank, score } from '../src/palette.ts';

const e = (label: string, key = label) => ({ key, label, section: 'Commands' });

test('a word-start match ranks above a match inside a word', () => {
  assert.deepEqual(rank([e('a thread'), e('Ready')], 'rea').map((x) => x.label), ['Ready', 'a thread']);
});

test('a dropped letter or odd spacing still finds the command', () => {
  assert.ok(score('Show keyboard shortcuts', 'shw kbd') >= 0);
  assert.ok(score('Search items', '  SEARCH ') >= 0);
  assert.equal(score('Search items', 'xyz'), -1);
});

test('an empty query keeps the given order', () => {
  assert.deepEqual(rank([e('b'), e('a')], '').map((x) => x.label), ['b', 'a']);
});

test('an item is found by its id as well as its title', () => {
  assert.deepEqual(rank([e('reads miss a fresh write', 'item:r1'), e('other', 'item:c2')], 'r1').map((x) => x.key), ['item:r1']);
});
