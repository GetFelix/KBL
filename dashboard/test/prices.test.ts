import assert from 'node:assert/strict';
import { test } from 'node:test';
import { dayMs, priceAgeDays, priceModels, priceRequest, priceText, stalePrices, unitPer, type PriceEntry } from '../src/spend.ts';

const DAY = 86_400_000;
const entry = (model: string, unit: PriceEntry['unit'], price_micros: number, effective_from_ms: number, as_of?: string): PriceEntry => ({ model, unit, price_micros, effective_from_ms, ...(as_of ? { as_of } : {}) });

test('a price older than the age is flagged, a newer or scheduled one is not', () => {
  const now = 1_000 * DAY;
  const prices = [
    entry('old', 'input', 1, now - 100 * DAY),
    entry('old', 'output', 1, now - 10 * DAY),
    entry('new', 'input', 1, now - 10 * DAY),
    entry('soon', 'input', 1, now + 5 * DAY),
  ];
  assert.deepEqual([...stalePrices({ prices }, now, 90)], [['old', 100]]);
  assert.deepEqual([...stalePrices({}, now, 90)], []);
});

test('staleness counts from the table\'s as_of day, not from when it was imported', () => {
  const now = dayMs('2026-11-15')!;
  // Imported just now from a table dated 200 days ago.
  const prices = [entry('m', 'input', 1, now, '2026-05-02'), entry('n', 'input', 1, now, '2026-11-01')];
  assert.deepEqual([...stalePrices({ prices }, now, 90)], [['m', 197]]);
});

test('only the price in effect counts: an old one that was replaced does not make a model stale', () => {
  const now = 1_000 * DAY;
  const prices = [entry('m', 'input', 1, now - 400 * DAY), entry('m', 'input', 2, now - 1 * DAY)];
  assert.deepEqual([...stalePrices({ prices }, now, 90)], []);
});

test('the price age comes from the address, else 90 days', () => {
  assert.equal(priceAgeDays(null), 90);
  assert.equal(priceAgeDays('30'), 30);
  for (const bad of ['0', '-1', 'x', '1.5', '']) assert.equal(priceAgeDays(bad), 90, bad);
});

test('a price typed in dollars becomes micros, and a bad one says why', () => {
  assert.deepEqual(priceRequest('m', 'input', '3'), { path: '/v1/work/prices', body: { model: 'm', unit: 'input', price_micros: 3_000_000 } });
  assert.deepEqual(priceRequest('m', 'output', ' 0.25 '), { path: '/v1/work/prices', body: { model: 'm', unit: 'output', price_micros: 250_000 } });
  for (const bad of ['', 'abc', '-1', '1.1234567']) assert.equal(typeof priceRequest('m', 'input', bad), 'string', bad);
});

test('a price of 0 is a price, as in the import; blank is not', () => {
  assert.deepEqual(priceRequest('m', 'input', '0'), { path: '/v1/work/prices', body: { model: 'm', unit: 'input', price_micros: 0 } });
  assert.equal(typeof priceRequest('m', 'input', ''), 'string');
});

test('a day ahead is sent as a price for later; a past or near day means now', () => {
  const now = dayMs('2026-11-15')! + 12 * 3_600_000;
  const body = (from: string) => (priceRequest('m', 'input', '1', from, now) as { body: Record<string, unknown> }).body;
  assert.equal(body('2026-12-01').effective_from_ms, dayMs('2026-12-01'));
  assert.equal(body('2026-01-01').effective_from_ms, undefined);
  assert.equal(body('').effective_from_ms, undefined);
  // Midnight just ahead of this clock: the tracker's clock may already be past it.
  assert.equal((priceRequest('m', 'input', '1', '2026-11-16', dayMs('2026-11-16')! - 60_000) as { body: Record<string, unknown> }).body.effective_from_ms, undefined);
  assert.equal(typeof priceRequest('m', 'input', '1', 'next week', now), 'string');
  assert.equal(typeof priceRequest('m', 'input', '1', '2026-02-30', now), 'string');
});

test('the price text names each unit in effect', () => {
  assert.equal(priceText({ prices: [entry('m', 'cache_read', 300_000, 0)] }, 'm', 10), 'cache read $0.30');
});

test('wording follows the unit: tokens per million, credits per credit', () => {
  assert.equal(unitPer('cache_write'), 'million cache write tokens');
  assert.equal(unitPer('aic'), 'AI credit');
});

test('the models page lists every model seen in usage or priced, each unit, never a fixed list', () => {
  const m = { input: 1000, output: 0, cache_read: 0, cache_write: 2000, aic_milli: 0, cost_micros: 0, unpriced: true };
  const g = { input: 0, output: 0, cache_read: 0, cache_write: 0, aic_milli: 2500, cost_micros: 0, unpriced: true };
  const s = {
    total_micros: 0, by_root: {}, by_review_tier: {}, by_repo: {},
    by_model: { seen: m, gpt: g },
    unpriced: [{ model: 'seen', unit: 'cache_write' as const }, { model: 'gpt', unit: 'aic' as const }],
    prices: [entry('seen', 'input', 3_000_000, 5, '2026-11-15'), entry('seen', 'input', 4_000_000, 100 * DAY), entry('priced-only', 'output', 1, 5)],
  };
  const list = priceModels(s, 10 * DAY);
  assert.deepEqual(list.map((p) => p.model), ['gpt', 'priced-only', 'seen']);
  const seen = list.find((p) => p.model === 'seen')!;
  const row = (unit: string) => seen.rows.find((r) => r.unit === unit)!;
  assert.equal(row('input').current?.price_micros, 3_000_000);
  assert.deepEqual(row('input').scheduled.map((p) => p.price_micros), [4_000_000]);
  assert.equal(row('input').usage, 1000);
  assert.deepEqual([row('cache_write').current, row('cache_write').usage, row('cache_write').waiting], [null, 2000, true]);
  assert.equal(seen.history.length, 2);
  // Credits show for a model that used them, and not for one that did not.
  assert.ok(list.find((p) => p.model === 'gpt')!.rows.some((r) => r.unit === 'aic' && r.usage === 2.5 && r.waiting));
  assert.ok(!seen.rows.some((r) => r.unit === 'aic'));
  assert.deepEqual(priceModels({ ...s, by_model: {}, unpriced: [], prices: [] }, 0), []);
});

test('Models and prices is a palette command, so keyboard users reach it', async () => {
  const { COMMANDS } = await import('../src/keys.ts');
  assert.ok(COMMANDS.some((c: { id: string; group: string }) => c.id === 'prices' && c.group === 'View'));
});
