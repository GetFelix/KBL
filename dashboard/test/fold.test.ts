import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { test } from 'node:test';
import { blocked, waitsOn, type Fold } from '../src/fold.ts';

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

test('a blocked item names a blocker in another repo with that repo', () => {
  const fold = new Folder();
  const rec = (o: object) => new TextEncoder().encode(JSON.stringify({ at_ms: 1, by: 'gabriel', actor: 'person', ...o }));
  const imp = (item: string, upstream_id: string) => ({ kind: 'imported', item, source: 'github', upstream_id, title: item });
  fold.apply(0, rec(imp('blk', 'GetFelix/felix-blocks#40')));
  fold.apply(1, rec(imp('gw', 'GetFelix/felix-gateway#31')));
  fold.apply(2, rec(imp('blk2', 'GetFelix/felix-blocks#41')));
  fold.apply(3, rec({ kind: 'linked', item: 'blk', to: 'gw' }));
  fold.apply(4, rec({ kind: 'linked', item: 'blk', to: 'blk2' }));
  const sink = { blockedJson: () => fold.blocked_json() } as Fold;
  const rows = blocked(sink);
  assert.deepEqual(rows.map((r) => r.id), ['blk']);
  assert.equal(waitsOn(rows[0]), 'blk2, gw (github:GetFelix/felix-gateway)');
});
