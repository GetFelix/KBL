// Flow, rework, cost and supervision numbers for the Metrics dialog. The fold computes them
// from accepted records (see docs/metrics.md); this file only checks the shape and words the
// figures. A team figure: nothing here names an agent, a model's user or a person. No DOM here.
import type { Fold } from './fold.ts';
import { dollars } from './spend.ts';

export interface StageTime {
  stage: string;
  n: number;
  p50_ms: number | null;
  p85_ms: number | null;
}

export interface MetricsReport {
  days: number;
  /** The log kept too few stage times to cover the whole window. */
  partial: boolean;
  /** The browser clock was behind the newest record, so the window ends at that record. */
  clock_behind: boolean;
  stages: StageTime[];
  throughput: { done: number; per_day: number[]; per_week: number[] };
  rework: { done: number; redos: number; reopened: number; failed_attempts: number; redos_per_done: number | null; reopened_per_done: number | null; failed_attempts_per_done: number | null };
  cost: { done: number; total_micros: number; per_done_micros: number | null; unattributed_micros: number; by_model: { model: string; cost_micros: number; per_done_micros: number | null; unpriced: boolean }[] };
  supervision: { done: number; touched: number; touches: number; touches_per_done: number | null; touched_share_permille: number | null; wait_on_person: { n: number; p50_ms: number | null; p85_ms: number | null } };
}

export function metrics(fold: Pick<Fold, 'metricsJson'>, nowMs: number, days: number): MetricsReport | null {
  try {
    const r = JSON.parse(fold.metricsJson(nowMs, days)) as MetricsReport;
    return Array.isArray(r.stages) && Array.isArray(r.throughput?.per_day) ? r : null;
  } catch {
    return null;
  }
}

const STAGE_NAMES: Record<string, string> = {
  proposed: 'Proposed',
  triage: 'Triage',
  needs_you: 'Needs you',
  blocked: 'Blocked',
  ready: 'Ready',
  held: 'Held by an agent',
  in_review: 'In review',
};

/** A span as its two largest units: `45s`, `12m`, `3h 20m`, `2d 4h`. Null is no figure. */
export function duration(ms: number | null): string {
  if (ms === null) return '—';
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d}d ${h % 24}h` : `${d}d`;
}

/** A per-item average with one decimal, or a dash with nothing done. */
export function perItem(x: number | null): string {
  return x === null ? '—' : (Math.round(x * 10) / 10).toLocaleString('en-US', { minimumFractionDigits: 1, maximumFractionDigits: 1 });
}

export function money(micros: number | null): string {
  return micros === null ? '—' : dollars(micros);
}

export interface Row {
  label: string;
  cells: string[];
}

export interface MetricsText {
  window: string;
  /** Said when nothing finished in the window, so every table is empty. */
  empty: string | null;
  partial: string | null;
  clockBehind: string | null;
  stages: Row[];
  throughput: { summary: string; weeks: Row[]; perDay: number[]; dayLabel: string };
  rework: Row[];
  cost: { summary: string; rows: Row[]; note: string | null };
  supervision: Row[];
}

const plural = (n: number, one: string, many: string) => `${n.toLocaleString('en-US')} ${n === 1 ? one : many}`;

export function metricsText(r: MetricsReport): MetricsText {
  const window = r.days === 1 ? 'the last day' : `the last ${r.days} days`;
  const done = r.throughput.done;
  const weeks = r.throughput.per_week;
  const peak = Math.max(0, ...r.throughput.per_day);
  const unpriced = r.cost.by_model.some((m) => m.unpriced);
  return {
    window,
    empty: done === 0 ? `No items finished in ${window}, so the per-item figures below are empty.` : null,
    partial: r.partial ? 'The log kept too few stage times to reach the start of this window, so the oldest are missing.' : null,
    clockBehind: r.clock_behind ? 'This device\'s clock is behind the newest records, so the window ends at the latest of them. Check the date and time on this device.' : null,
    stages: r.stages.map((s) => ({ label: STAGE_NAMES[s.stage] ?? s.stage, cells: [s.n.toLocaleString('en-US'), duration(s.p50_ms), duration(s.p85_ms)] })),
    throughput: {
      summary: `${plural(done, 'item', 'items')} done in ${window}`,
      weeks: weeks.map((n, i) => ({ label: weeks.length - 1 - i === 0 ? 'Latest 7 days' : `${weeks.length - 1 - i} ${weeks.length - 1 - i === 1 ? 'week' : 'weeks'} earlier`, cells: [n.toLocaleString('en-US')] })),
      perDay: r.throughput.per_day,
      dayLabel: `Items done per day over ${window}, oldest first; the busiest day had ${peak.toLocaleString('en-US')}.`,
    },
    rework: [
      { label: 'Redos', cells: [r.rework.redos.toLocaleString('en-US'), perItem(r.rework.redos_per_done)] },
      { label: 'Reopened', cells: [r.rework.reopened.toLocaleString('en-US'), perItem(r.rework.reopened_per_done)] },
      { label: 'Failed attempts', cells: [r.rework.failed_attempts.toLocaleString('en-US'), perItem(r.rework.failed_attempts_per_done)] },
    ],
    cost: {
      summary: done === 0 ? 'Nothing done to spread a cost over' : `${money(r.cost.per_done_micros)} per done item (${money(r.cost.total_micros)} over ${plural(done, 'item', 'items')})`,
      rows: r.cost.by_model.map((m) => ({ label: m.model, cells: [money(m.cost_micros), money(m.per_done_micros)] })),
      note: [
        r.cost.unattributed_micros > 0 ? `${money(r.cost.unattributed_micros)} was reported with no model, so it is in the total only.` : null,
        unpriced ? 'Some usage has no price yet, so the cost of those models is a floor.' : null,
      ]
        .filter((x) => x !== null)
        .join(' ') || null,
    },
    supervision: [
      { label: 'Items a person touched', cells: [`${r.supervision.touched.toLocaleString('en-US')} of ${r.supervision.done.toLocaleString('en-US')}`, r.supervision.touched_share_permille === null ? '—' : `${Math.floor(r.supervision.touched_share_permille / 10)}%`] },
      { label: 'Records a person wrote', cells: [r.supervision.touches.toLocaleString('en-US'), `${perItem(r.supervision.touches_per_done)} per done item`] },
      { label: 'Waiting on a person, per done item', cells: [`${duration(r.supervision.wait_on_person.p50_ms)} p50`, `${duration(r.supervision.wait_on_person.p85_ms)} p85`] },
    ],
  };
}
