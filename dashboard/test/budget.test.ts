import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { test } from 'node:test';
import { bigLog } from '../check/fixture.mjs';
import { ITEMS } from '../check/budget.mjs';

const { Folder } = createRequire(import.meta.url)('../.wasm-node/kbl_wasm.js');

const encoder = new TextEncoder();
const HANG_MS = 10_000;
// A linear fold costs the same per record at any size; quadratic work grows it with size.
const MAX_PER_RECORD_GROWTH = 4;
const SMALL_ITEMS = 1000;

function timeFold(items: number): { perRecordMs: number; rows: number; ms: number } {
  const payloads = (bigLog(items) as object[]).map((r) => encoder.encode(JSON.stringify(r)));
  const fold = new Folder();
  const start = performance.now();
  payloads.forEach((p, i) => assert.ok(fold.apply(i, p), `record ${i}`));
  const rows = JSON.parse(fold.items_json());
  const ms = performance.now() - start;
  return { perRecordMs: ms / payloads.length, rows: rows.length, ms };
}

// Runner speed scales both folds alike, so the ratio holds on a slow runner where a fixed
// millisecond budget does not.
test(`folding ${ITEMS} items costs about the same per record as folding ${SMALL_ITEMS}`, () => {
  timeFold(SMALL_ITEMS); // warm up the wasm module
  const small = Math.min(...[1, 2, 3].map(() => timeFold(SMALL_ITEMS).perRecordMs));
  const big = timeFold(ITEMS);
  console.log(`# fold of ${ITEMS} items: ${big.ms.toFixed(0)} ms, per record ${(big.perRecordMs / small).toFixed(2)}x the ${SMALL_ITEMS}-item fold`);
  assert.equal(big.rows, ITEMS);
  assert.ok(big.ms < HANG_MS, `${big.ms.toFixed(0)} ms: the fold hung`);
  assert.ok(
    big.perRecordMs < small * MAX_PER_RECORD_GROWTH,
    `per-record cost grew ${(big.perRecordMs / small).toFixed(1)}x from ${SMALL_ITEMS} to ${ITEMS} items (limit ${MAX_PER_RECORD_GROWTH}x)`,
  );
});
