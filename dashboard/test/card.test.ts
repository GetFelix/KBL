import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ago, chipText, childIndex, lead, properties, reasonLine, relatedLine, summarize, upstreamRef, upstreamUrl, workingLine } from '../src/card.ts';
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
  assert.equal(workingLine(held, 1_000_000 + 3 * 60_000), 'agent-1 working, attempt 2, last active 3 min ago');
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
