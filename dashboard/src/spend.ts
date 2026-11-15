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
  /** Models with usage that has no price yet. */
  unpriced: string[];
}

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
    .map(([key, micros]) => ({ key, micros, ...describe(key), percent: total > 0 ? Math.max(1, Math.round((micros / total) * 100)) : 0 }))
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
  const waiting = new Set(s.unpriced ?? []);
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
