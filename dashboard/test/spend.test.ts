import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { test } from 'node:test';
import { dollars, spend, tables } from '../src/spend.ts';
import { COMMANDS } from '../src/keys.ts';

// Built by `npm run wasm`.
const { Folder } = createRequire(import.meta.url)('../.wasm-node/kbl_wasm.js');

const enc = (o: object) => new TextEncoder().encode(JSON.stringify({ at_ms: 1, by: 'gabriel', actor: 'person', ...o }));

/** A log where r1 (felix) cost 400 and r2 (gateway) cost 1000, as in the core fold's spend test. */
function folded() {
  const fold = new Folder();
  let at = 0;
  const apply = (o: object) => assert.ok(fold.apply(at++, enc(o)));
  const agent = (o: object) => apply({ actor: 'agent', by: 'w1', ...o });
  for (const [item, repo] of [['r1', 'GetFelix/felix#1'], ['r2', 'GetFelix/felix-gateway#3']]) {
    apply({ kind: 'imported', item, source: 'github', upstream_id: repo, title: `title ${item}`, actor: 'tracker', by: 'ingest' });
    apply({ kind: 'accepted', item, tier: 1 });
    agent({ kind: 'claimed', item, attempt: 1, deadline_ms: 1800000090000 });
  }
  agent({ kind: 'done', item: 'r1', attempt: 1, cost_micros: 400 });
  agent({ kind: 'done', item: 'r2', attempt: 1, cost_micros: 1000 });
  return fold;
}

test('spend reads the fold per root and per repo, never estimated', () => {
  const s = spend({ spendJson: () => folded().spend_json() });
  assert.equal(s.total_micros, 1400);
  assert.deepEqual(s.by_root, { r1: 400, r2: 1000 });
  assert.deepEqual(s.by_repo, { 'github:GetFelix/felix': 400, 'github:GetFelix/felix-gateway': 1000 });
  assert.deepEqual(s.by_review_tier, {});
});

test('tables sort largest first, name a root by its title, and split a repo from its source', () => {
  const s = spend({ spendJson: () => folded().spend_json() });
  const t = tables(s, [{ id: 'r1', title: 'Fix the lease', stage: 'done' }]);
  assert.deepEqual(t.roots.map((r) => [r.key, r.label, r.hint, r.micros, r.percent]), [
    ['r2', 'r2', '', 1000, 71],
    ['r1', 'Fix the lease', 'r1', 400, 29],
  ]);
  assert.deepEqual(t.repos.map((r) => [r.label, r.hint]), [['GetFelix/felix-gateway', 'github'], ['GetFelix/felix', 'github']]);
  assert.deepEqual(t.tiers, []);
});

test('reviewer tiers are listed as recorded, with their share of the total', () => {
  const t = tables({ total_micros: 400, by_root: {}, by_review_tier: { fast: 100, deep: 300 }, by_repo: {}, by_model: {}, unpriced: [] }, []);
  assert.deepEqual(t.tiers.map((r) => [r.label, r.micros, r.percent]), [['deep', 300, 75], ['fast', 100, 25]]);
});

test('a row that spent anything never rounds to 0 percent, and nothing spent shows none', () => {
  const t = tables({ total_micros: 100_000, by_root: { a: 99_999, b: 1 }, by_review_tier: {}, by_repo: {}, by_model: {}, unpriced: [] }, []);
  assert.equal(t.roots[1].percent, 1);
  assert.equal(tables({ total_micros: 0, by_root: {}, by_review_tier: {}, by_repo: {}, by_model: {}, unpriced: [] }, []).total, 0);
});

/** A format 8 log: opus is priced, `gpt-6` (AI credits) has no price yet, and each report has a 10 cent floor. */
function priced() {
  const fold = new Folder();
  let at = 0;
  const apply = (o: object) => assert.ok(fold.apply(at++, enc({ v: 8, ...o })));
  apply({ kind: 'format_raised', to: 8 });
  apply({ kind: 'price_set', model: 'opus', unit: 'input', price_micros: 15_000_000 });
  apply({ kind: 'price_set', model: 'opus', unit: 'output', price_micros: 75_000_000 });
  for (const [item, usage] of [
    ['r1', { unit: 'tokens', model: 'opus', input: 20_000, output: 4_000 }],
    ['r2', { unit: 'aic', model: 'gpt-6', milli: 1_500 }],
  ] as const) {
    apply({ kind: 'imported', item, source: 'github', upstream_id: `GetFelix/felix#${item.slice(1)}`, title: item, actor: 'tracker', by: 'ingest' });
    apply({ kind: 'accepted', item, tier: 1 });
    apply({ actor: 'agent', by: 'w1', kind: 'claimed', item, attempt: 1, deadline_ms: 1800000090000 });
    apply({ actor: 'agent', by: 'w1', kind: 'done', item, attempt: 1, cost_micros: 0, usage: [usage], floor_micros: 100_000 });
  }
  return fold;
}

test('a priced model shows its usage and derived cost', () => {
  const t = tables(spend({ spendJson: () => priced().spend_json() }), []);
  const opus = t.models.find((m) => m.model === 'opus');
  assert.deepEqual(opus, { model: 'opus', unit: 'tokens', usage: '20,000 input, 4,000 output', micros: 600_000, unpriced: false });
});

test('an unpriced model shows its usage and no cost, and is never counted as $0', () => {
  const t = tables(spend({ spendJson: () => priced().spend_json() }), []);
  const gpt = t.models.find((m) => m.model === 'gpt-6');
  assert.deepEqual(gpt, { model: 'gpt-6', unit: 'AIC', usage: '1.5 credits', micros: 0, unpriced: true });
  // The report is charged its floor, so the total is more than the priced sum.
  assert.equal(t.total, 600_000 + 100_000);
});

test('a model waiting for a price is listed even without usage detail', () => {
  const t = tables({ total_micros: 0, by_root: {}, by_review_tier: {}, by_repo: {}, by_model: {}, unpriced: ['x'] }, []);
  assert.deepEqual(t.models.map((m) => [m.model, m.unpriced, m.usage]), [['x', true, '']]);
});

test('a model with tokens and credits, partly priced, shows both units and a partial cost', () => {
  const m = { input: 1000, output: 0, cache_read: 0, cache_write: 0, aic_milli: 2500, cost_micros: 15_000, unpriced: true };
  const t = tables({ total_micros: 100_000, by_root: {}, by_review_tier: {}, by_repo: {}, by_model: { mix: m }, unpriced: ['mix'] }, []);
  assert.deepEqual(t.models, [{ model: 'mix', unit: 'tokens + AIC', usage: '1,000 input; 2.5 credits', micros: 15_000, unpriced: true }]);
});

test('a root that cost nothing shows no share, not 1 percent', () => {
  const t = tables({ total_micros: 100, by_root: { a: 100, b: 0 }, by_review_tier: {}, by_repo: {}, by_model: {}, unpriced: [] }, []);
  assert.equal(t.roots.find((r) => r.key === 'b')?.percent, 0);
});

test('dollars shows cents, thousands, and sub-cent amounts', () => {
  assert.equal(dollars(0), '$0.00');
  assert.equal(dollars(1_250_000), '$1.25');
  assert.equal(dollars(1_234_567_000), '$1,234.57');
  assert.equal(dollars(400), '$0.0004');
  assert.equal(dollars(9_999), '$0.01');
  assert.equal(dollars(1), '<$0.0001');
  assert.equal(dollars(100), '$0.0001');
});

test('Spend is a palette command, so keyboard users reach it', () => {
  assert.ok(COMMANDS.some((c) => c.id === 'spend' && c.group === 'View'));
});
