import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ago, blockText, chipText, childIndex, lead, parsePr, properties, reasonLine, relatedLine, summarize, upstreamRef, upstreamUrl, workingLine } from '../src/card.ts';
import type { ItemRow } from '../src/fold.ts';

const root: ItemRow = { id: 'g0378517ddd9edf0f', title: 'Reads miss', stage: 'ready', upstream: 'GetFelix/felix#1694', source: 'github', root: true };
const triage: ItemRow = { id: 'g0378-t', title: 'Triage', stage: 'done', parent: root.id, work: 'triage', upstream: root.upstream, source: 'github' };
const augment: ItemRow = { id: 'g0378-a', title: 'Augment', stage: 'needs_you', parent: root.id, work: 'augment', upstream: root.upstream, source: 'github', needs: 'question' };

test('a card leads with the upstream reference, a child with its kind, else the KBL id', () => {
  assert.equal(upstreamRef(root), 'felix#1694');
  assert.equal(lead(root), 'felix#1694');
  assert.equal(lead(triage), 'felix#1694 triage');
  assert.equal(lead({ id: 'x1', title: 't', stage: 'ready' }), 'x1');
  assert.equal(upstreamRef({ upstream: 'ADO-12' }), 'ADO-12');
});

test('the upstream link needs a github source and a repository in the id', () => {
  assert.equal(upstreamUrl(root), 'https://github.com/GetFelix/felix/issues/1694');
  assert.equal(upstreamUrl({ upstream: 'GetFelix/felix#1', source: 'jira' }), null);
  assert.equal(upstreamUrl({ upstream: 'ADO-12', source: 'github' }), null);
});

test('children roll up under their top root with a progress chip', () => {
  const grand: ItemRow = { id: 'g', title: 'g', stage: 'ready', parent: triage.id, work: 'fix' };
  const idx = childIndex([root, triage, augment, grand]);
  assert.deepEqual(idx.get(root.id)?.map((i) => i.id), ['g0378-t', 'g0378-a', 'g']);
  const c = summarize(idx.get(root.id)!);
  assert.equal(chipText(c), '3 sub-tasks, 1 done, 1 need you');
  assert.equal(chipText(summarize([triage])), '1 sub-task, 1 done');
});

test('the working line names the agent, attempt and time since it last wrote', () => {
  const held: ItemRow = { ...root, holder: 'agent-1', held_attempt: 2, active_ms: 1_000_000 };
  assert.equal(workingLine(held, 1_000_000 + 3 * 60_000), 'agent-1 working, attempt 2, last tracker write 3 min ago');
  // The supervisor's recorded heartbeat tells a busy worker from a stuck one.
  const now = 1_000_000 + 6 * 60_000;
  assert.equal(workingLine({ ...held, holder_alive_ms: now - 30_000 }, now), 'agent-1 working, attempt 2, last tracker write 6 min ago, process running');
  assert.equal(workingLine({ ...held, holder_alive_ms: now - 5 * 60_000 }, now), 'agent-1 working, attempt 2, last tracker write 6 min ago, no heartbeat for 5 min');
  assert.equal(workingLine(root, 0), null);
  assert.equal(ago(0, 30_000), 'just now');
  assert.equal(ago(0, 2 * 3_600_000), '2 h ago');
});

test('a card says what it waits on: an edge, a failed check, or a person', () => {
  assert.equal(reasonLine(root, 'gw-31'), 'waits on gw-31');
  assert.equal(reasonLine({ ...root, needs: 'check_failed', failing_check: 'clippy' }, undefined), 'Check failed: clippy');
  assert.equal(reasonLine(augment, undefined), 'An agent asked a question');
  assert.equal(reasonLine(root, undefined), '');
});

test('properties list tier, source and the pull request with its checks', () => {
  assert.deepEqual(properties({ ...root, tier: 2, pr: 'GetFelix/felix#2300', pr_phase: 'open', pr_checks: 'red' }), ['Tier 2', 'github', 'PR #2300, checks fail']);
  assert.deepEqual(properties({ id: 'a', title: 'a', stage: 'ready' }), []);
  assert.deepEqual(properties({ id: 'a', title: 'a', stage: 'ready', tier: 1, milestone: '0.8', class: { item_type: { value: 'bug', by: 'triage' }, size: { value: 'M', by: 'person' } } }), ['Tier 1', '0.8', 'bug', 'M']);
  assert.deepEqual(properties({ id: 'a', title: 'a', stage: 'ready', class: { needs: { values: ['gpu', 'azure'], by: 'person' } } }), ['Needs gpu, azure']);
  assert.deepEqual(properties({ id: 'a', title: 'a', stage: 'ready', class: { needs: { values: [], by: 'person' } } }), []);
});

test('a block shows its resource, its end and its reason until it ends', () => {
  const now = Date.parse('2026-11-18T12:00:00Z');
  const until = Date.parse('2026-11-21T12:00:00Z');
  const row = { ...root, block: { at_ms: now, by: 'gab', until_ms: until, resource: 'azure-vms', reason: 'no credit' } };
  assert.match(blockText(row, now) ?? '', /^Blocked on azure-vms until .+: no credit$/);
  assert.equal(blockText({ block: { at_ms: now, by: 'gab', resource: 'gpu', reason: 'busy' } }, now), 'Blocked on gpu: busy');
  assert.equal(blockText(row, until), null);
  assert.equal(blockText(root, now), null);
  assert.equal(properties(row, now).at(-1), blockText(row, now));
  assert.ok(!properties(row).some((x) => x.startsWith('Blocked')));
});

test('a screen reader name is the upstream reference, the id when there is none', async () => {
  const { nameOf } = await import('../src/card.ts');
  const items = [{ id: 'gh-1', title: 't', stage: 'ready', upstream: 'GetFelix/felix#1694' }, { id: 'loc-2', title: 't', stage: 'ready' }];
  assert.equal(nameOf(items, 'gh-1'), 'felix#1694');
  assert.equal(nameOf(items, 'loc-2'), 'loc-2');
  assert.equal(nameOf(items, 'gone'), 'gone');
});

test('a related item is named on both cards and is not a reason the card waits', () => {
  const a: ItemRow = { id: 'a', title: 'a', stage: 'ready', upstream: 'GetFelix/felix#1505', related: ['b'] };
  const b: ItemRow = { id: 'b', title: 'b', stage: 'ready', upstream: 'GetFelix/felix#376', related: ['a'] };
  assert.equal(relatedLine(a, [a, b]), 'Related: felix#376');
  assert.equal(relatedLine(b, [a, b]), 'Related: felix#1505');
  assert.equal(relatedLine({}, [a, b]), null);
  assert.equal(relatedLine({ related: [] }, [a, b]), null);
  assert.equal(reasonLine(a, undefined), '', 'a relation is not a wait');
  assert.equal(relatedLine({ related: ['w', 'x', 'y', 'z', 'q'] }, []), 'Related: w, x, y and 2 more');
});

test('a pull request address reads owner/repo#N, never a URL after a hash', () => {
  assert.deepEqual(parsePr('https://github.com/GetFelix/felix/pull/2859'), { label: 'GetFelix/felix#2859', url: 'https://github.com/GetFelix/felix/pull/2859' });
  assert.deepEqual(parsePr('GetFelix/felix#2300'), { label: '#2300', url: 'https://github.com/GetFelix/felix/pull/2300' });
  assert.equal(parsePr('not a reference').url, null);
  assert.equal(properties({ ...root, pr: 'https://github.com/GetFelix/felix/pull/2859' }).at(-1), 'PR GetFelix/felix#2859');
});
