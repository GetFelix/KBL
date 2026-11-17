import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { test } from 'node:test';
import { bigLog } from '../check/fixture.mjs';
import { FOLD_BUDGET_MS, ITEMS } from '../check/budget.mjs';

const { Folder } = createRequire(import.meta.url)('../.wasm-node/kbl_wasm.js');

test(`folding ${ITEMS} items and reading them back stays inside ${FOLD_BUDGET_MS.node} ms`, () => {
  const encoder = new TextEncoder();
  const payloads = (bigLog(ITEMS) as object[]).map((r) => encoder.encode(JSON.stringify(r)));
  const fold = new Folder();
  const start = performance.now();
  payloads.forEach((p, i) => assert.ok(fold.apply(i, p), `record ${i}`));
  const rows = JSON.parse(fold.items_json());
  const ms = performance.now() - start;
  console.log(`# fold of ${payloads.length} records, ${rows.length} items: ${ms.toFixed(0)} ms`);
  assert.equal(rows.length, ITEMS);
  assert.ok(ms < FOLD_BUDGET_MS.node, `${ms.toFixed(0)} ms over the ${FOLD_BUDGET_MS.node} ms budget`);
});
