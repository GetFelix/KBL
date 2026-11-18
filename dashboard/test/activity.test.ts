import assert from 'node:assert/strict';
import { test } from 'node:test';
import { history, linked, money, nowLine, since, timeline, type ActivityEvent } from '../src/activity.ts';
import type { ItemRow } from '../src/fold.ts';

const item = (o: Partial<ItemRow> = {}): ItemRow => ({ id: 'a', title: 'A', stage: 'ready', ...o });
const ev = (o: Partial<ActivityEvent>): ActivityEvent => ({ offset: 1, at_ms: 0, by: 'w1', kind: 'claimed', ...o });
const NOW = 1_000_000;

test('since says how long ago, and how far ahead', () => {
  assert.equal(since(NOW - 2000, NOW), 'just now');
  assert.equal(since(NOW - 40_000, NOW), '40 s ago');
  assert.equal(since(NOW - 180_000, NOW), '3 min ago');
  assert.equal(since(NOW + 50_000, NOW), 'in 50 s');
});

test('the Now line names the holder, the attempt, the last activity and the claim deadline', () => {
  const held = item({ stage: 'held', holder: 'agent1', held_attempt: 2, active_ms: NOW - 40_000, hold_deadline_ms: NOW + 50_000 });
  const n = nowLine(held, [], [], NOW);
  assert.equal(n.text, 'agent1 is working on this, attempt 2, last activity 40 s ago. Claim deadline in 50 s.');
  assert.equal(n.tone, 'ok');
  const late = nowLine({ ...held, hold_deadline_ms: NOW - 5_000 }, [], [], NOW);
  assert.equal(late.tone, 'warn');
  assert.match(late.text, /claim deadline passed 5 s ago/);
});

test('the Now line says what a waiting item waits on', () => {
  assert.match(nowLine(item({ stage: 'needs_you', needs: 'question' }), [], [], NOW).text, /^Waiting for a person: /);
  const row = { id: 'a', title: 'A', repo: null, source: null, waits_on: [{ id: 'b', title: 'B', repo: null, stage: 'ready', source: null, awaiting_release: false }] };
  assert.equal(nowLine(item({ stage: 'blocked' }), [row], [], NOW).text, 'Blocked, waiting on b.');
  assert.equal(nowLine(item({ stage: 'in_review', pr: 'o/r#3', pr_checks: 'pending' }), [], [], NOW).tone, 'plain');
  assert.equal(nowLine(item(), [], [], NOW).text, 'Ready. No agent has claimed it yet.');
});

test('the timeline is newest first and marks a failed step', () => {
  const t = timeline([ev({ offset: 1 }), ev({ offset: 2, kind: 'step', command: 'cargo test', exit_code: 101, output: 'boom', attempt: 1 }), ev({ offset: 3, kind: 'done', attempt: 1 })]);
  assert.deepEqual(t.map((e) => e.offset), [3, 2, 1]);
  assert.equal(t[1].title, 'Ran cargo test');
  assert.equal(t[1].failed, true);
  assert.equal(t[1].output, 'boom');
  assert.equal(t[1].who, 'w1, attempt 1');
});

test('history gives each attempt its outcome, summary, mark and cost', () => {
  const rows = history(
    [
      ev({ offset: 1, kind: 'claimed', attempt: 1 }),
      ev({ offset: 2, kind: 'progress', attempt: 1, text: 'cache fill moved' }),
      ev({ offset: 3, kind: 'step', attempt: 1, exit_code: 1 }),
      ev({ offset: 4, kind: 'released', attempt: 1, why: 'failed', reason: 'tests red', cost_micros: 300_000 }),
      ev({ offset: 5, kind: 'claimed', attempt: 2 }),
      ev({ offset: 6, kind: 'done', attempt: 2, cost_micros: 1_250_000 }),
      ev({ offset: 7, kind: 'rated', attempt: 2, accurate: true }),
    ],
    item(),
  );
  assert.deepEqual(rows.map((r) => [r.attempt, r.outcome, r.accurate, r.cost_micros, r.failedSteps]), [
    [1, 'Released: it failed, tests red', null, 300_000, 1],
    [2, 'Finished', true, 1_250_000, 0],
  ]);
  assert.equal(rows[0].summary, 'cache fill moved');
  assert.equal(money(1_250_000), '$1.25');
  assert.equal(money(1_000), 'under $0.01');
});

test('a question parks the attempt, but the held attempt still reads as working', () => {
  const events = [ev({ attempt: 1 }), ev({ offset: 2, kind: 'question', attempt: 1 })];
  assert.equal(history(events, item())[0].outcome, 'Waiting on a person');
  assert.equal(history(events, item({ holder: 'w1', held_attempt: 1 }))[0].outcome, 'Working');
});

test('linked work lists children with their answers, what the item waits on, and what it blocks', () => {
  const kid = item({ id: 'k', parent: 'a', work: 'triage', stage: 'done', title: 'Triage' });
  const blockedBy = { id: 'z', title: 'Z', repo: null, source: null, waits_on: [{ id: 'a', title: 'A', repo: null, stage: 'ready', source: null, awaiting_release: false }] };
  const acts = new Map([['k', [ev({ kind: 'answer', chosen: 'Rust client' })]]]);
  const l = linked(item(), [item(), kid], [blockedBy], acts);
  assert.deepEqual(l.children, [{ id: 'k', work: 'triage', title: 'Triage', stage: 'done', answer: 'Rust client' }]);
  assert.deepEqual(l.blocks, [{ id: 'z', title: 'Z' }]);
});
