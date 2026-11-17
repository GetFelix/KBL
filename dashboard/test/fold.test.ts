import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { test } from 'node:test';
import { blocked, reviewLoad, waitsOn, type Fold, type ItemRow } from '../src/fold.ts';
import { MAX_FILES, MAX_NOTES, overlapNotes } from '../src/overlap.ts';

// Built by `npm run wasm`.
const { Folder } = createRequire(import.meta.url)('../.wasm-node/kbl_wasm.js');
const golden = new URL('../../wasm/golden/', import.meta.url);

for (const name of ['every_record', 'pre_prerequisite', 'pre_questions', 'format_2', 'format_3', 'format_4', 'format_5', 'format_6', 'format_7', 'agent_context']) {
  test(`the wasm fold equals the native fold on ${name}`, () => {
    const log = readFileSync(new URL(`${name}.jsonl`, golden), 'utf8').trimEnd().split('\n');
    const fold = new Folder();
    log.forEach((line, i) => assert.ok(fold.apply(i, new TextEncoder().encode(line)), `record ${i}`));
    // The expected file is written by the native fold (wasm/tests/golden.rs).
    assert.equal(fold.state_json(), readFileSync(new URL(`${name}.state.json`, golden), 'utf8'));
    assert.equal(fold.at(), log.length - 1);
  });
}

const rec = (o: object) => new TextEncoder().encode(JSON.stringify({ at_ms: 1, by: 'gabriel', actor: 'person', ...o }));
const agent = (o: object) => new TextEncoder().encode(JSON.stringify({ at_ms: 1, by: 'w1', actor: 'agent', ...o }));
const imp = (item: string, source: string, upstream_id: string) => ({ kind: 'imported', item, source, upstream_id, title: item });

test('items lists each item with its stage', () => {
  const fold = new Folder();
  const rec = (o: object) => new TextEncoder().encode(JSON.stringify({ at_ms: 1, by: 'gabriel', actor: 'person', ...o }));
  fold.apply(0, rec({ kind: 'imported', item: 'r1', source: 'github', upstream_id: 'a#1', title: 'first' }));
  assert.deepEqual(JSON.parse(fold.items_json()), [
    { id: 'r1', title: 'first', stage: 'triage', tier: null, proposed_tier: null, source: 'github', repo: null, needs: null, question: null, options: null, default: null, unclear: null, root: true, accepted: false, flagged: false },
  ]);
  fold.apply(1, rec({ kind: 'accepted', item: 'r1', tier: 1 }));
  assert.equal(JSON.parse(fold.items_json())[0].stage, 'ready');
  assert.equal(JSON.parse(fold.items_json())[0].tier, 1);
});

test('an open question carries its offset, options and default, and why the item needs a person', () => {
  const fold = new Folder();
  const rec = (o: object) => new TextEncoder().encode(JSON.stringify({ at_ms: 1, by: 'gabriel', actor: 'person', ...o }));
  fold.apply(0, rec({ kind: 'imported', item: 'r1', source: 'github', upstream_id: 'a#1', title: 'first' }));
  fold.apply(1, rec({ kind: 'accepted', item: 'r1', tier: 1 }));
  fold.apply(2, rec({ kind: 'claimed', actor: 'agent', by: 'a1', item: 'r1', attempt: 1, deadline_ms: 9e12 }));
  fold.apply(3, rec({ kind: 'question', actor: 'agent', by: 'a1', item: 'r1', attempt: 1, text: { text: 'Which?' }, options: ['one', 'two'], default: 'two' }));
  const [r1] = JSON.parse(fold.items_json());
  assert.deepEqual([r1.stage, r1.needs, r1.question, r1.options, r1.default], ['needs_you', 'question', 3, ['one', 'two'], 'two']);
});

test('items carry the repo an owner/repo#n id names, and children inherit it from the root', () => {
  const fold = new Folder();
  const rec = (o: object) => new TextEncoder().encode(JSON.stringify({ at_ms: 1, by: 'gabriel', actor: 'person', ...o }));
  fold.apply(0, rec({ kind: 'imported', item: 'r1', source: 'github', upstream_id: 'GetFelix/KBL#7', title: 'first' }));
  fold.apply(1, rec({ kind: 'accepted', item: 'r1', tier: 2 }));
  fold.apply(2, rec({ kind: 'proposed', item: 'c1', parent: 'r1', key: 'k1', work: 'split', title: 'child' }));
  const byId = Object.fromEntries(JSON.parse(fold.items_json()).map((i: { id: string }) => [i.id, i]));
  assert.deepEqual([byId.r1.source, byId.r1.repo, byId.r1.tier], ['github', 'GetFelix/KBL', 2]);
  assert.deepEqual([byId.c1.source, byId.c1.repo], ['github', 'GetFelix/KBL']);
});

test('a blocked item names a blocker in another repo with that repo', () => {
  const fold = new Folder();
  fold.apply(0, rec(imp('blk', 'github', 'GetFelix/felix-blocks#40')));
  fold.apply(1, rec(imp('gw', 'github', 'GetFelix/felix-gateway#31')));
  fold.apply(2, rec(imp('blk2', 'github', 'GetFelix/felix-blocks#41')));
  fold.apply(3, rec({ kind: 'accepted', item: 'blk', tier: 1 }));
  fold.apply(4, rec({ kind: 'linked', item: 'blk', to: 'gw' }));
  fold.apply(5, rec({ kind: 'linked', item: 'blk', to: 'blk2' }));
  const sink = { blockedJson: () => fold.blocked_json() } as Fold;
  const rows = blocked(sink);
  assert.deepEqual(rows.map((r) => r.id), ['blk']);
  assert.equal(waitsOn(rows[0]), 'blk2, gw (github:GetFelix/felix-gateway)');
});

test('a blocker with no repo shows its source', () => {
  const fold = new Folder();
  fold.apply(0, rec(imp('blk', 'github', 'GetFelix/felix-blocks#40')));
  fold.apply(1, rec(imp('j1', 'jira', 'site/FEL-123')));
  fold.apply(2, rec({ kind: 'accepted', item: 'blk', tier: 1 }));
  fold.apply(3, rec({ kind: 'linked', item: 'blk', to: 'j1' }));
  const rows = blocked({ blockedJson: () => fold.blocked_json() } as Fold);
  assert.equal(waitsOn(rows[0]), 'j1 (jira)');
});

test('a jira row does not repeat its own source on a jira blocker', () => {
  const fold = new Folder();
  fold.apply(0, rec(imp('j1', 'jira', 'site/FEL-1')));
  fold.apply(1, rec(imp('j2', 'jira', 'site/FEL-2')));
  fold.apply(2, rec({ kind: 'accepted', item: 'j1', tier: 1 }));
  fold.apply(3, rec({ kind: 'linked', item: 'j1', to: 'j2' }));
  const rows = blocked({ blockedJson: () => fold.blocked_json() } as Fold);
  assert.equal(waitsOn(rows[0]), 'j2');
});

test('a blocker merged but awaiting its release says so', () => {
  const fold = new Folder();
  fold.apply(0, rec(imp('r1', 'github', 'a/b#1')));
  fold.apply(1, rec(imp('r2', 'github', 'a/b#2')));
  fold.apply(2, rec({ kind: 'accepted', item: 'r1' }));
  fold.apply(3, agent({ kind: 'claimed', item: 'r1', attempt: 1, deadline_ms: 1800000090000 }));
  fold.apply(4, agent({ kind: 'prerequisite', item: 'r1', attempt: 1, prerequisite: 'r2', key: 'need', done_when: 'released', cost_micros: 0 }));
  fold.apply(5, rec({ kind: 'closed', item: 'r2', why: 'merged' }));
  const rows = blocked({ blockedJson: () => fold.blocked_json() } as Fold);
  assert.equal(waitsOn(rows[0]), 'r2 release');
});

test('blocked lists only blocked items, not triage or held ones', () => {
  const fold = new Folder();
  fold.apply(0, rec(imp('dep', 'github', 'a/b#1')));
  fold.apply(1, rec(imp('tri', 'github', 'a/b#2')));
  fold.apply(2, rec(imp('hld', 'github', 'a/b#3')));
  fold.apply(3, rec(imp('blk', 'github', 'a/b#4')));
  fold.apply(4, rec({ kind: 'accepted', item: 'hld' }));
  fold.apply(5, rec({ kind: 'accepted', item: 'blk' }));
  fold.apply(6, agent({ kind: 'claimed', item: 'hld', attempt: 1, deadline_ms: 1800000090000 }));
  for (const [i, item] of ['tri', 'hld', 'blk'].entries()) fold.apply(7 + i, rec({ kind: 'linked', item, to: 'dep' }));
  const rows = blocked({ blockedJson: () => fold.blocked_json() } as Fold);
  assert.deepEqual(rows.map((r) => r.id), ['blk']);
});

test('both cards of an overlap get a note with the suggested link', () => {
  const fold = new Folder();
  let at = 0;
  const apply = (o: object) =>
    assert.ok(fold.apply(at++, new TextEncoder().encode(JSON.stringify({ at_ms: 1, by: 'gabriel', actor: 'person', ...o }))));
  for (const item of ['r1', 'r2']) {
    apply({ kind: 'imported', item, source: 'github', upstream_id: `GetFelix/felix#${item}`, title: item, by: 'ingest', actor: 'tracker' });
    apply({ kind: 'accepted', item, tier: 1 });
  }
  apply({ kind: 'context', item: 'r1', entry: 'files', text: { text: 'core/src/work.rs' } });
  apply({ kind: 'context', item: 'r2', entry: 'files', text: { text: 'core/src/' } });
  const notes = overlapNotes({ overlapsJson: () => fold.overlaps_json() });
  const suggest = 'r2 waits on r1';
  const files = ['core/src/work.rs'];
  assert.deepEqual(notes.get('r1'), { notes: [{ other: 'r2', files, moreFiles: 0, suggest }], more: 0 });
  assert.deepEqual(notes.get('r2'), { notes: [{ other: 'r1', files, moreFiles: 0, suggest }], more: 0 });
});

test('a card holds at most MAX_NOTES notes of at most MAX_FILES paths', () => {
  const many = Array.from({ length: MAX_FILES + 3 }, (_, i) => `f${i}.rs`);
  const overlaps = Array.from({ length: MAX_NOTES + 2 }, (_, i) => ({ first: 'r0', then: `r${i + 1}`, files: many }));
  const notes = overlapNotes({ overlapsJson: () => JSON.stringify(overlaps) });
  const card = notes.get('r0')!;
  assert.equal(card.notes.length, MAX_NOTES);
  assert.equal(card.more, 2);
  assert.equal(card.notes[0].files.length, MAX_FILES);
  assert.equal(card.notes[0].moreFiles, 3);
  assert.equal(notes.get('r1')!.notes.length, 1);
});

test('items say whether a person accepted and flagged them', () => {
  const fold = new Folder();
  fold.apply(0, rec(imp('r1', 'github', 'a#1')));
  assert.deepEqual(JSON.parse(fold.items_json()).map((i: ItemRow) => [i.accepted, i.flagged]), [[false, false]]);
  fold.apply(1, rec({ kind: 'accepted', item: 'r1', tier: 1 }));
  // A flag is a format 7 record: the log is raised first.
  fold.apply(2, rec({ v: 7, kind: 'format_raised', to: 7 }));
  fold.apply(3, rec({ v: 7, kind: 'flagged', item: 'r1', flagged: true }));
  const [r1] = JSON.parse(fold.items_json());
  assert.deepEqual([r1.accepted, r1.flagged, r1.needs, r1.stage], [true, true, 'flagged', 'needs_you']);
  fold.apply(4, rec({ v: 7, kind: 'flagged', item: 'r1', flagged: false }));
  assert.deepEqual([JSON.parse(fold.items_json())[0].flagged, JSON.parse(fold.items_json())[0].needs], [false, null]);
});

test('the review load the page reads is the one the tracker pause counts', () => {
  const fold = new Folder();
  const rec = (at: number, r: object) => assert.ok(fold.apply(at, new TextEncoder().encode(JSON.stringify({ at_ms: at, by: 'gabriel', actor: 'person', ...r }))));
  let at = 0;
  for (const id of ['a', 'b', 'c']) {
    rec(at++, { kind: 'imported', item: id, source: 'github', upstream_id: `x/y#${id}`, title: id, by: 'ingest', actor: 'tracker' });
    rec(at++, { kind: 'accepted', item: id });
    rec(at++, { kind: 'claimed', item: id, attempt: 1, deadline_ms: 1800000090000, by: 'w1', actor: 'agent' });
  }
  for (const id of ['a', 'b']) rec(at++, { kind: 'done', item: id, attempt: 1, cost_micros: 0, by: 'w1', actor: 'agent' });
  assert.deepEqual(reviewLoad({ reviewLoadJson: () => fold.review_load_json() }), { load: 3, inReview: 2 });
});
