import assert from 'node:assert/strict';
import test from 'node:test';
import type { ItemRow } from '../src/fold.ts';
import { checkTries, checksText, diffText, failedText, handoffKey, handoffs, markText, marks, prUrl, type Handoff } from '../src/handoff.ts';

const ho = (o: Partial<Handoff> = {}): Handoff => ({ attempt: 2, summary: 'fixed it', files: 3, added: 42, removed: 7, partial: false, failed_steps: [], failed_total: 0, accurate: null, ...o });
const item = (o: Partial<ItemRow> = {}): ItemRow => ({ id: 'r1', title: 'r1', stage: 'in_review', format: 15, ...o });

test('handoffs are keyed by item', () => {
  const got = handoffs({ handoffsJson: () => JSON.stringify({ r1: ho() }) });
  assert.equal(got.get('r1')?.attempt, 2);
  assert.equal(got.get('r2'), undefined);
});

test('the diff is the recorded edits, and says so when it records none or only part', () => {
  assert.equal(diffText(ho()), 'Logged edits: 3 files, +42 -7');
  assert.equal(diffText(ho({ files: 1, added: 1, removed: 0 })), 'Logged edits: 1 file, +1 -0');
  assert.equal(diffText(ho({ files: 0, added: 0, removed: 0 })), 'No edits logged');
  assert.match(diffText(ho({ partial: true })), /at least \+42 -7 \(some hunks are stored outside the log\)/);
});

test('checks are shown only when the pull request reports them', () => {
  assert.equal(checksText(item()), null);
  assert.equal(checksText(item({ pr: 'GetFelix/felix#9' })), null);
  assert.equal(checksText(item({ pr: 'GetFelix/felix#9', pr_checks: 'green' })), 'Checks pass');
  assert.equal(checksText(item({ pr: 'GetFelix/felix#9', pr_checks: 'red', failing_check: 'lint' })), 'Checks fail: lint');
  assert.equal(checksText(item({ pr: 'GetFelix/felix#9', pr_checks: 'pending' })), 'Checks running');
});

test('the pull request links only when it names a repository', () => {
  assert.equal(prUrl('GetFelix/felix#9'), 'https://github.com/GetFelix/felix/pull/9');
  assert.equal(prUrl('#9'), null);
  assert.equal(prUrl(null), null);
});

test('failed steps are counted, and a capped list says how many are not shown', () => {
  assert.equal(failedText(ho()), null);
  assert.equal(failedText(ho({ failed_total: 1, failed_steps: [{ offset: 5, command: 'cargo test', exit_code: 101 }] })), '1 step failed');
  assert.equal(failedText(ho({ failed_total: 12, failed_steps: Array.from({ length: 10 }, (_, i) => ({ offset: i, command: 'x', exit_code: 1 })) })), '12 steps failed (2 not listed)');
});

test('the recorded mark is the pressed one, and nothing is pressed before a mark', () => {
  assert.deepEqual(marks(ho()).map((m) => m.pressed), [false, false]);
  assert.deepEqual(marks(ho({ accurate: false })).map((m) => [m.label, m.pressed]), [['Accurate', false], ['Not accurate', true]]);
  assert.equal(markText(ho()), 'Summary not marked yet');
  assert.equal(markText(ho({ accurate: true })), 'Summary marked accurate');
});

test('a block is rebuilt when the item, the attempt, the mark or the checks change', () => {
  const base = handoffKey(item(), ho());
  assert.equal(handoffKey(item(), undefined), '');
  assert.notEqual(base, handoffKey(item({ id: 'r2' }), ho()));
  assert.notEqual(base, handoffKey(item(), ho({ attempt: 3 })));
  assert.notEqual(base, handoffKey(item(), ho({ accurate: true })));
  assert.notEqual(base, handoffKey(item({ pr_checks: 'red' }), ho()));
  assert.equal(base, handoffKey(item(), ho()));
});

test('a root shows every failed try with its check, head, fix and log', () => {
  const rows = checkTries(item({ check_tries: [
    { offset: 7, check: 'lint', head: 'abcdef123', url: 'https://x/job/1', fix: 'r1-f1', log: 'error: unused\n' },
    { offset: 9, check: 'lint', head: 'def4567', url: 'https://x/job/2', fix: null, log: null },
  ] }));
  assert.deepEqual(rows.map((r) => r.title), ['Try 1: lint on abcdef1, fix r1-f1', 'Try 2: lint on def4567, no fix opened']);
  assert.equal(rows[0]!.log, 'error: unused');
  assert.equal(rows[1]!.log, 'No log excerpt was recorded.');
  assert.deepEqual(checkTries(item()), []);
});
