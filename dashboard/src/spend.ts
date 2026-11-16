// What claims cost, shaped for the Spend dialog. The numbers are the fold's `spend()`
// and nothing is estimated here. See docs/work-tracker.md "Where work comes from, and its bounds". No DOM here.
import type { Fold, ItemRow } from './fold.ts';

/** The fold's spend, in millionths of a dollar. */
export interface Spend {
  total_micros: number;
  /** Per root, over the root and everything under it. */
  by_root: Record<string, number>;
  /** Reviews only, per the reviewer tier each one asked for. */
  by_review_tier: Record<string, number>;
  /** Per `source:repo` of the item's root. */
  by_repo: Record<string, number>;
  /** Usage per model and what its priced part cost. The total is more than their sum by floors. */
  by_model: Record<string, ModelSpend>;
  /** The units of models whose usage has no price yet. */
  unpriced: ModelUnit[];
  /** Every price set, by model, unit and start. Absent on an older fold. */
  prices?: PriceEntry[];
}

export interface ModelUnit {
  model: string;
  unit: PriceUnit;
}

export interface PriceEntry {
  model: string;
  unit: PriceUnit;
  price_micros: number;
  /** When it starts, in milliseconds since the epoch. */
  effective_from_ms: number;
  /** The vendor day (`YYYY-MM-DD`) an import copied it from; absent for a price a person typed. */
  as_of?: string;
}

export type PriceUnit = 'input' | 'output' | 'cache_read' | 'cache_write' | 'aic';

/** One model's usage; `aic_milli` is thousandths of an AI credit. */
export interface ModelSpend {
  input: number;
  output: number;
  cache_read: number;
  cache_write: number;
  aic_milli: number;
  /** The cost of the usage that has a price, in millionths of a dollar. */
  cost_micros: number;
  /** Some of the usage has no price yet. */
  unpriced: boolean;
}

export function spend(fold: Pick<Fold, 'spendJson'>): Spend {
  return JSON.parse(fold.spendJson()) as Spend;
}

/** One line of a table: what was spent on a root, a reviewer tier or a repo. */
export interface SpendRow {
  /** The root's id, the tier, or `source:repo`. */
  key: string;
  label: string;
  /** Where the label comes from, such as the source of a repo; empty when there is nothing to add. */
  hint: string;
  micros: number;
  /** Whole percent of the total, never below 1 for a row that spent anything. */
  percent: number;
}

/** One model of the By model table. `unpriced` models show their usage and no cost. */
export interface ModelRow {
  model: string;
  /** `tokens`, `AIC`, or both. */
  unit: string;
  usage: string;
  /** Cost of the priced part; 0 with `unpriced` means there is no cost to show. */
  micros: number;
  unpriced: boolean;
}

export interface SpendTables {
  total: number;
  roots: SpendRow[];
  tiers: SpendRow[];
  repos: SpendRow[];
  models: ModelRow[];
}

const FORMAT = new Intl.NumberFormat('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });

/** Millionths of a dollar as text. Under a cent it keeps up to four decimals, and under a hundredth of a cent it says `<$0.0001`, so a small claim is not shown as nothing. */
export function dollars(micros: number): string {
  if (micros > 0 && micros < 10_000) return micros < 100 ? '<$0.0001' : `$${(micros / 1e6).toFixed(4).replace(/0+$/, '')}`;
  return `$${FORMAT.format(micros / 1e6)}`;
}

function rows(map: Record<string, number>, total: number, describe: (key: string) => { label: string; hint: string }): SpendRow[] {
  return Object.entries(map)
    .map(([key, micros]) => ({ key, micros, ...describe(key), percent: total > 0 && micros > 0 ? Math.max(1, Math.round((micros / total) * 100)) : 0 }))
    .sort((a, b) => b.micros - a.micros || a.key.localeCompare(b.key));
}

const COUNT = new Intl.NumberFormat('en-US', { maximumFractionDigits: 3 });

function modelRow(model: string, m: ModelSpend | undefined, waiting: boolean): ModelRow {
  const parts: string[] = [];
  const units: string[] = [];
  if (m) {
    const tokens = [['input', m.input], ['output', m.output], ['cache read', m.cache_read], ['cache write', m.cache_write]] as const;
    const used = tokens.filter(([, n]) => n > 0);
    if (used.length) {
      units.push('tokens');
      parts.push(used.map(([name, n]) => `${COUNT.format(n)} ${name}`).join(', '));
    }
    if (m.aic_milli > 0) {
      units.push('AIC');
      parts.push(`${COUNT.format(m.aic_milli / 1000)} credits`);
    }
  }
  return { model, unit: units.join(' + '), usage: parts.join('; '), micros: m?.cost_micros ?? 0, unpriced: waiting || !!m?.unpriced };
}

/** The four tables, largest first. A root shows its title when the log has the item, else its id. */
export function tables(s: Spend, items: readonly ItemRow[]): SpendTables {
  const titles = new Map(items.map((i) => [i.id, i.title]));
  const total = s.total_micros;
  const byModel = s.by_model ?? {};
  const waiting = new Set((s.unpriced ?? []).map((u) => u.model));
  const models = [...new Set([...Object.keys(byModel), ...waiting])]
    .map((name) => modelRow(name, byModel[name], waiting.has(name)))
    .sort((a, b) => b.micros - a.micros || a.model.localeCompare(b.model));
  return {
    total,
    roots: rows(s.by_root, total, (id) => ({ label: titles.get(id) ?? id, hint: titles.has(id) ? id : '' })),
    tiers: rows(s.by_review_tier, total, (tier) => ({ label: tier, hint: '' })),
    repos: rows(s.by_repo, total, (key) => {
      const at = key.indexOf(':');
      return at < 0 ? { label: key, hint: '' } : { label: key.slice(at + 1), hint: key.slice(0, at) };
    }),
    models,
  };
}

/** A price older than this many days gets a "check pricing" hint. `?price_age_days=N` changes it. */
export const DEFAULT_PRICE_AGE_DAYS = 90;
const DAY_MS = 86_400_000;
/** A day this close to now (or past) is written as now, as the import does: the tracker's clock differs from this one. */
const NOW_MARGIN_MS = 10 * 60 * 1000;

/** The age in days from the address's `price_age_days`, or the default when it is missing or not a whole number above 0. */
export function priceAgeDays(raw: string | null): number {
  const n = raw === null || !/^\d+$/.test(raw) ? 0 : Number(raw);
  return n > 0 ? n : DEFAULT_PRICE_AGE_DAYS;
}

/** `YYYY-MM-DD` as milliseconds at 00:00 UTC, or null when it is not a real day. */
export function dayMs(day: string): number | null {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(day)) return null;
  const ms = Date.parse(`${day}T00:00:00Z`);
  return Number.isNaN(ms) || new Date(ms).toISOString().slice(0, 10) !== day ? null : ms;
}

const WORDS: Record<PriceUnit, [short: string, per: string]> = {
  input: ['Input', 'million input tokens'],
  output: ['Output', 'million output tokens'],
  cache_read: ['Cache read', 'million cache read tokens'],
  cache_write: ['Cache write', 'million cache write tokens'],
  aic: ['AI credits', 'AI credit'],
};

/** The unit's name in a table, such as `Cache read`. */
export function unitName(unit: PriceUnit): string {
  return WORDS[unit][0];
}

/** What a price is per, such as `million input tokens` or `AI credit`. */
export function unitPer(unit: PriceUnit): string {
  return WORDS[unit][1];
}

/** The price in effect at `now` for each model and unit: the latest entry that has started. */
export function inEffect(s: Pick<Spend, 'prices'>, now: number): Map<string, PriceEntry> {
  const out = new Map<string, PriceEntry>();
  for (const p of s.prices ?? []) {
    const key = `${p.model}\0${p.unit}`;
    if (p.effective_from_ms <= now && p.effective_from_ms >= (out.get(key)?.effective_from_ms ?? -1)) out.set(key, p);
  }
  return out;
}

/** When a price was current, in ms: the vendor day an import copied it from, else when it started. */
export function pricedAt(p: PriceEntry): number {
  return (p.as_of ? dayMs(p.as_of) : null) ?? p.effective_from_ms;
}

/** Where a price came from, in words. */
export function sourceText(p: PriceEntry): string {
  return p.as_of ? `import, as of ${p.as_of}` : 'person';
}

/** The models whose oldest price in effect was current more than `ageDays` before `now`, each with that age in whole days. A price that has not started is not old. No block, only a hint. */
export function stalePrices(s: Pick<Spend, 'prices'>, now: number, ageDays: number): Map<string, number> {
  const out = new Map<string, number>();
  for (const p of inEffect(s, now).values()) {
    const age = Math.floor((now - pricedAt(p)) / DAY_MS);
    if (age > ageDays && age > (out.get(p.model) ?? -1)) out.set(p.model, age);
  }
  return out;
}

/** The prices a model has in effect, as `input $3 / output $15` per million tokens (per credit for `aic`), for the hint. */
export function priceText(s: Pick<Spend, 'prices'>, model: string, now: number): string {
  return [...inEffect(s, now).values()]
    .filter((p) => p.model === model)
    .map((p) => `${unitName(p.unit).toLowerCase()} ${dollars(p.price_micros)}`)
    .join(' / ');
}

/** One unit of one model on the Models and prices page. Only recorded fields: nothing here is a guess. */
export interface PriceRow {
  model: string;
  unit: PriceUnit;
  /** In effect now, if any. */
  current: PriceEntry | null;
  /** Set for later, soonest first. */
  scheduled: PriceEntry[];
  /** Usage accrued in this unit: tokens, or credits for `aic`. 0 when none was reported. */
  usage: number;
  /** Usage of this unit is waiting for a price. */
  waiting: boolean;
}

export interface PriceModel {
  model: string;
  rows: PriceRow[];
  /** Every price set for the model, oldest start first. */
  history: PriceEntry[];
}

const TOKEN_UNITS: PriceUnit[] = ['input', 'output', 'cache_read', 'cache_write'];

function used(m: ModelSpend | undefined, unit: PriceUnit): number {
  if (!m) return 0;
  return unit === 'aic' ? m.aic_milli / 1000 : m[unit];
}

/** Every model seen in recorded usage or given a price, priced or not, with its units. The token units always show so any of them can be priced; credits show when used or priced. */
export function priceModels(s: Spend, now: number): PriceModel[] {
  const entries = s.prices ?? [];
  const current = inEffect(s, now);
  const waiting = new Set((s.unpriced ?? []).map((u) => `${u.model}\0${u.unit}`));
  const names = new Set([...Object.keys(s.by_model ?? {}), ...(s.unpriced ?? []).map((u) => u.model), ...entries.map((p) => p.model)]);
  return [...names].sort().map((model) => {
    const m = (s.by_model ?? {})[model];
    const mine = entries.filter((p) => p.model === model);
    const units = TOKEN_UNITS.concat(used(m, 'aic') > 0 || mine.some((p) => p.unit === 'aic') || waiting.has(`${model}\0aic`) ? ['aic'] : []);
    return {
      model,
      rows: units.map((unit) => ({
        model,
        unit,
        current: current.get(`${model}\0${unit}`) ?? null,
        scheduled: mine.filter((p) => p.unit === unit && p.effective_from_ms > now).sort((a, b) => a.effective_from_ms - b.effective_from_ms),
        usage: used(m, unit),
        waiting: waiting.has(`${model}\0${unit}`),
      })),
      history: [...mine].sort((a, b) => a.effective_from_ms - b.effective_from_ms || TOKEN_UNITS.concat(['aic']).indexOf(a.unit) - TOKEN_UNITS.concat(['aic']).indexOf(b.unit)),
    };
  });
}

/** The price typed in dollars as micros, or why it cannot be. 0 is a price: a model that costs nothing is set to 0, not left blank. */
export function parseDollars(dollarsText: string): number | string {
  const text = dollarsText.trim();
  if (!/^\d+(\.\d{1,6})?$/.test(text)) return 'Type the price in dollars, such as 3 or 0.25.';
  const [whole, frac = ''] = text.split('.');
  const micros = Number(whole) * 1_000_000 + Number(frac.padEnd(6, '0'));
  return Number.isSafeInteger(micros) ? micros : 'That price is too large.';
}

/** When a price typed with this optional UTC day starts: the day if it has not come yet, else now. Null when the day is not a real one. */
export function priceStart(from: string, now: number): number | null {
  if (from.trim() === '') return now;
  const ms = dayMs(from.trim());
  return ms === null ? null : ms > now + NOW_MARGIN_MS ? ms : now;
}

/** The price a new one would replace: the one in effect when it starts, which for a later day is not the one in effect now. */
export function replacedPrice(s: Pick<Spend, 'prices'>, model: string, unit: PriceUnit, from: string, now: number): PriceEntry | null {
  const at = priceStart(from, now) ?? now;
  let out: PriceEntry | null = null;
  for (const p of s.prices ?? []) {
    if (p.model === model && p.unit === unit && p.effective_from_ms <= at && p.effective_from_ms >= (out?.effective_from_ms ?? -1)) out = p;
  }
  return out;
}

/** Whether two answers to "what would this replace" are the same price, so a confirm of one still holds for the other. */
export function samePrice(a: PriceEntry | null, b: PriceEntry | null): boolean {
  return a === null || b === null ? a === b : a.price_micros === b.price_micros && a.effective_from_ms === b.effective_from_ms;
}

/**
 * The `POST /v1/work/prices` request for a price typed in dollars (per million tokens, or per credit for `aic`),
 * or why it cannot be sent. `from` is an optional UTC day: one that has not come yet is sent as a price for later,
 * a day that has passed or is near means now, since a price cannot reach back.
 */
export function priceRequest(model: string, unit: PriceUnit, dollarsText: string, from = '', now = Date.now()): { path: string; body: Record<string, unknown> } | string {
  const micros = parseDollars(dollarsText);
  if (typeof micros === 'string') return micros;
  const body: Record<string, unknown> = { model, unit, price_micros: micros };
  if (from.trim() !== '') {
    const ms = dayMs(from.trim());
    if (ms === null) return 'Type the day as YYYY-MM-DD, such as 2026-12-01.';
    if (ms > now + NOW_MARGIN_MS) body.effective_from_ms = ms;
  }
  return { path: '/v1/work/prices', body };
}
