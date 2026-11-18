import assert from 'node:assert/strict';
import test from 'node:test';
import { duration, metrics, metricsText, money, perItem, type MetricsReport } from '../src/metrics.ts';

const report = (o: Partial<MetricsReport> = {}): MetricsReport => ({
  days: 30,
  partial: false,
  stages: [
    { stage: 'triage', n: 4, p50_ms: 3_600_000, p85_ms: 90_000_000 },
    { stage: 'in_review', n: 0, p50_ms: null, p85_ms: null },
  ],
  throughput: { done: 9, per_day: [0, 3, 6], per_week: [2, 7] },
  rework: { done: 9, redos: 3, reopened: 1, failed_attempts: 4, redos_per_done: 1 / 3, reopened_per_done: 1 / 9, failed_attempts_per_done: 4 / 9 },
  cost: { done: 9, total_micros: 4_500_000, per_done_micros: 500_000, unattributed_micros: 250_000, by_model: [{ model: 'm1', cost_micros: 4_250_000, per_done_micros: 472_222, unpriced: true }] },
  supervision: { done: 9, touched: 6, touches: 15, touches_per_done: 15 / 9, touched_share_permille: 666, wait_on_person: { n: 9, p50_ms: 120_000, p85_ms: 7_200_000 } },
  ...o,
});

test('spans read in their two largest units, and no figure is a dash', () => {
  assert.deepEqual([0, 45_000, 720_000, 12_000_000, 7_200_000, 90_000_000, 86_400_000].map(duration), ['0s', '45s', '12m', '3h 20m', '2h', '1d 1h', '1d']);
  assert.equal(duration(null), '—');
});

test('averages carry one decimal and money comes from the spend formatter', () => {
  assert.equal(perItem(1 / 3), '0.3');
  assert.equal(perItem(null), '—');
  assert.equal(money(500_000), '$0.50');
  assert.equal(money(null), '—');
});

test('the dialog words every section from the report', () => {
  const x = metricsText(report());
  assert.deepEqual(x.stages[0], { label: 'Triage', cells: ['4', '1h', '1d 1h'] });
  assert.deepEqual(x.stages[1].cells, ['0', '—', '—'], 'a stage nobody stayed in shows dashes, not zero time');
  assert.equal(x.throughput.summary, '9 items done in the last 30 days');
  assert.deepEqual(x.throughput.weeks.map((r) => r.label), ['1 week earlier', 'Latest 7 days']);
  assert.deepEqual(x.rework[0].cells, ['3', '0.3']);
  assert.equal(x.cost.summary, '$0.50 per done item ($4.50 over 9 items)');
  assert.match(x.cost.note ?? '', /no model/);
  assert.match(x.cost.note ?? '', /no price yet/);
  assert.deepEqual(x.supervision[0].cells, ['6 of 9', '66%']);
  assert.deepEqual(x.supervision[2].cells, ['2m p50', '2h p85']);
  assert.equal(x.empty, null);
});

test('nothing done says so and shows no invented averages', () => {
  const x = metricsText(report({ throughput: { done: 0, per_day: [0], per_week: [0] }, rework: { done: 0, redos: 0, reopened: 0, failed_attempts: 0, redos_per_done: null, reopened_per_done: null, failed_attempts_per_done: null }, cost: { done: 0, total_micros: 0, per_done_micros: null, unattributed_micros: 0, by_model: [] }, days: 1 }));
  assert.match(x.empty ?? '', /No items finished in the last day/);
  assert.equal(x.rework[0].cells[1], '—');
  assert.equal(x.cost.summary, 'Nothing done to spread a cost over');
  assert.equal(x.cost.note, null);
});

test('a partial window is flagged', () => {
  assert.match(metricsText(report({ partial: true })).partial ?? '', /too few stage times/);
});

test('an answer that is not a report is refused', () => {
  assert.equal(metrics({ metricsJson: () => '{}' }, 0, 7), null);
  assert.equal(metrics({ metricsJson: () => 'nope' }, 0, 7), null);
  assert.equal(metrics({ metricsJson: () => JSON.stringify(report()) }, 0, 30)?.days, 30);
});
