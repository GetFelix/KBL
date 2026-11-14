import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { test } from 'node:test';
import { overlapNotes, type Fold } from '../src/fold.ts';

// Built by `npm run wasm`.
const { Folder } = createRequire(import.meta.url)('../.wasm-node/kbl_wasm.js');
const golden = new URL('../../wasm/golden/', import.meta.url);

for (const name of ['every_record', 'pre_prerequisite', 'pre_questions', 'format_2', 'format_3', 'format_4', 'agent_context']) {
  test(`the wasm fold equals the native fold on ${name}`, () => {
    const log = readFileSync(new URL(`${name}.jsonl`, golden), 'utf8').trimEnd().split('\n');
    const fold = new Folder();
    log.forEach((line, i) => assert.ok(fold.apply(i, new TextEncoder().encode(line)), `record ${i}`));
    // The expected file is written by the native fold (wasm/tests/golden.rs).
    assert.equal(fold.state_json(), readFileSync(new URL(`${name}.state.json`, golden), 'utf8'));
    assert.equal(fold.at(), log.length - 1);
  });
}

test('items lists each item with its stage', () => {
  const fold = new Folder();
  const rec = (o: object) => new TextEncoder().encode(JSON.stringify({ at_ms: 1, by: 'gabriel', actor: 'person', ...o }));
  fold.apply(0, rec({ kind: 'imported', item: 'r1', source: 'github', upstream_id: 'a#1', title: 'first' }));
  assert.deepEqual(JSON.parse(fold.items_json()), [{ id: 'r1', title: 'first', stage: 'triage' }]);
  fold.apply(1, rec({ kind: 'accepted', item: 'r1', tier: 1 }));
  assert.equal(JSON.parse(fold.items_json())[0].stage, 'ready');
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
  const notes = overlapNotes({ overlapsJson: () => fold.overlaps_json() } as Fold);
  const suggest = 'r2 waits on r1';
  assert.deepEqual(notes.get('r1'), [{ other: 'r2', files: ['core/src/work.rs'], suggest }]);
  assert.deepEqual(notes.get('r2'), [{ other: 'r1', files: ['core/src/work.rs'], suggest }]);
});
