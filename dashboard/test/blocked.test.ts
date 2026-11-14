import assert from 'node:assert/strict';
import { test } from 'node:test';
import { blockerLines, blockerNote, blockerText, waitsOn, type BlockedRow, type Blocker } from '../src/fold.ts';

const gh = (repo: string) => ({ source: 'github', repo });
const blocker = (o: Partial<Blocker> & { id: string }): Blocker => ({ title: null, repo: null, stage: 'triage', source: null, awaiting_release: false, ...o });
const row = (o: Partial<BlockedRow>, waits_on: Blocker[]): BlockedRow => ({ id: 'blk', title: 't', repo: gh('GetFelix/felix-blocks'), source: 'github', waits_on, ...o });

test('a blocker in the row repo shows no place; one in another repo shows that repo', () => {
  const r = row({}, [blocker({ id: 'blk2', repo: gh('GetFelix/felix-blocks'), source: 'github' }), blocker({ id: 'gw', repo: gh('GetFelix/felix-gateway'), source: 'github' })]);
  assert.deepEqual(blockerLines(r).map((b) => b.where), ['', 'github:GetFelix/felix-gateway']);
  assert.equal(waitsOn(r), 'blk2, gw (github:GetFelix/felix-gateway)');
});

test('a blocker with no repo shows its source only when the row has another', () => {
  assert.equal(waitsOn(row({}, [blocker({ id: 'j1', source: 'jira' })])), 'j1 (jira)');
  assert.equal(waitsOn(row({ repo: null, source: 'jira' }, [blocker({ id: 'j2', source: 'jira' })])), 'j2');
});

test('a blocker awaiting its release says so after the place', () => {
  const [b] = blockerLines(row({}, [blocker({ id: 'p1', source: 'jira', awaiting_release: true })]));
  assert.equal(blockerNote(b), '(jira) release');
  assert.equal(blockerText(b), 'p1 (jira) release');
  assert.equal(blockerText({ ...b, where: '' }), 'p1 release');
  assert.equal(blockerNote({ ...b, where: '', release: false }), '');
});
