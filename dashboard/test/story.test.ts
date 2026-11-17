import assert from 'node:assert/strict';
import { test } from 'node:test';
import { attemptFlags, citedSteps, citedText, contentText, decisionCount, flagsFor, stories, type Story } from '../src/story.ts';

const story: Story = {
  item: 'r1',
  rev: 3,
  attempts: [{ attempt: 1, forked_from: null, done: false, closed: false }],
  groups: [{ phase: 'fix', decisions: [{ offset: 7, attempt: 1, by: 'w1', so_far: 's', question: 'q', options: [], chosen: 'c', why: 'w', commit: 'abc', evidence: [], revert_check: null, proven: false, edits: [] }] }],
  people: [],
  flags: [
    { rule: 'fix_without_edit', attempt: 1, decision: 7, offsets: [], message: 'm' },
    { rule: 'edits_never_run', attempt: 1, decision: null, offsets: [8], message: 'n' },
  ],
};

test('flags split into those of a decision and those of an attempt', () => {
  assert.deepEqual(flagsFor(story, 7).map((f) => f.rule), ['fix_without_edit']);
  assert.deepEqual(flagsFor(story, 9), []);
  assert.deepEqual(attemptFlags(story).map((f) => f.rule), ['edits_never_run']);
  assert.equal(decisionCount(story), 1);
});

test('a cited offset reads as the record it points at, or just the offset', () => {
  assert.equal(citedText({ stream: 'work.events', offset: 3, found: { kind: 'step', command: 'cargo test', exit_code: 1 } }), 'step 3: cargo test (exit 1)');
  assert.equal(citedText({ stream: 'work.events', offset: 4, found: null }), 'offset 4');
});

test('content shows inline text and not an artifact', () => {
  assert.equal(contentText({ text: 'hi' }), 'hi');
  assert.equal(contentText({ artifact: 'sha256:x' }), null);
});

test('stories are keyed by item', () => {
  const got = stories({ storiesJson: () => JSON.stringify({ r1: story }) });
  assert.equal(got.get('r1')?.groups.length, 1);
});

test('a step cited by two decisions is listed once, in offset order', () => {
  const step = (offset: number, exit_code: number) => ({ stream: 'work.events', offset, found: { kind: 'step' as const, command: `run ${offset}`, exit_code } });
  const d = story.groups[0]!.decisions[0]!;
  const two: Story = {
    ...story,
    groups: [{ phase: 'fix', decisions: [{ ...d, evidence: [step(5, 0), step(3, 1)] }, { ...d, offset: 8, evidence: [step(3, 1), { stream: 'work.events', offset: 9, found: null }] }] }],
  };
  assert.deepEqual(citedSteps(two), [{ offset: 3, command: 'run 3', exit_code: 1 }, { offset: 5, command: 'run 5', exit_code: 0 }]);
});
