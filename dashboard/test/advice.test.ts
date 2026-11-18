import assert from 'node:assert/strict';
import test from 'node:test';
import { adviceChanged, adviceText, canAdvise, checkAdvice, parseDeps, parseSplit } from '../src/advice.ts';
import type { ItemRow } from '../src/fold.ts';
import { jobsFor, NO_WRITES, request, summary } from '../src/writes.ts';

const item = (o: Partial<ItemRow> & { id: string }): ItemRow => ({ title: o.id, stage: 'ready', tier: 1, format: 17, ...o });
const proposal = { at: 7, why: 'w', depends_on: ['b'], split: ['one'] };
const a = item({ id: 'a', stage: 'triage', tier: null, accepted: false, proposed_tier: 1, proposal });
const rows = [a, item({ id: 'b' }), item({ id: 'a-t', parent: 'a', work: 'triage' })];

test('fields become lists: ids by commas or spaces, titles by line, each once and never empty', () => {
  assert.deepEqual(parseDeps(' b,c  b\nd '), ['b', 'c', 'd']);
  assert.deepEqual(parseDeps(''), []);
  assert.deepEqual(parseSplit('one\n\n  two  \n'), ['one', 'two']);
  assert.deepEqual(adviceText(proposal), { deps: 'b', split: 'one' });
  assert.equal(adviceChanged(proposal, ['b'], ['one']), false);
  assert.equal(adviceChanged(proposal, [], ['one']), true);
});

test('only an unaccepted, answered proposal on a log at format 17 can be adjusted', () => {
  assert.equal(canAdvise(a), true);
  assert.equal(canAdvise({ ...a, format: 16 }), false);
  assert.equal(canAdvise({ ...a, accepted: true }), false);
  assert.equal(canAdvise({ ...a, stage: 'proposed' }), false);
  assert.equal(canAdvise({ ...a, proposal: null }), false);
});

test('advice naming itself, its triage work, nothing real or too much is caught before sending', () => {
  assert.equal(checkAdvice(a, ['b'], ['x'], rows), null);
  assert.match(checkAdvice(a, ['a'], [], rows)!, /itself/);
  assert.match(checkAdvice(a, ['a-t'], [], rows)!, /triage work/);
  assert.match(checkAdvice(a, ['zz'], [], rows)!, /not an item/);
  assert.match(checkAdvice(a, [], Array.from({ length: 9 }, (_, i) => `t${i}`), rows)!, /At most 8/);
  assert.match(checkAdvice(a, [], ['x'.repeat(300)], rows)!, /over 256/);
});

test('the edit goes to the proposals route for that item and names the answer it read', () => {
  assert.deepEqual(request('a', { kind: 'advice', depends_on: [], split: ['x'], proposal: 7 }), {
    path: '/v1/work/proposals/edit',
    body: { item: 'a', depends_on: [], split: ['x'], proposal: 7 },
  });
});

test('an edit against an answer triage has replaced since is refused unsent', () => {
  const args = { kind: 'advice' as const, depends_on: [], split: [], proposal: 7 };
  assert.deepEqual(jobsFor(['a'], args, rows, NO_WRITES)[0].req, request('a', args));
  const newer = rows.map((i) => (i.id === 'a' ? { ...i, proposal: { ...proposal, at: 9 } } : i));
  assert.match(jobsFor(['a'], args, newer, NO_WRITES)[0].req as string, /answered a again/);
  const accepted = rows.map((i) => (i.id === 'a' ? { ...i, accepted: true } : i));
  assert.match(jobsFor(['a'], args, accepted, NO_WRITES)[0].req as string, /nothing was sent/);
});

test('a refusal below the format says which format to raise to', () => {
  const s = summary([{ id: 1, batch: 1, kind: 'advice', item: 'a', status: 'refused', why: 'format', reason: 'x' }]);
  assert.match(s.text, /format 17/);
});
