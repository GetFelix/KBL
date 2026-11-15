// What claims cost, shaped for the Spend dialog. The numbers are the fold's `spend()`
// and nothing is estimated here. See docs/work-tracker.md "Spend". No DOM here.
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

export interface SpendTables {
  total: number;
  roots: SpendRow[];
  tiers: SpendRow[];
  repos: SpendRow[];
}

const FORMAT = new Intl.NumberFormat('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 });

/** Millionths of a dollar as text. Under a cent it keeps up to four decimals, so a small claim is not shown as nothing. */
export function dollars(micros: number): string {
  if (micros > 0 && micros < 10_000) return `$${(micros / 1e6).toFixed(4).replace(/0+$/, '')}`;
  return `$${FORMAT.format(micros / 1e6)}`;
}

function rows(map: Record<string, number>, total: number, describe: (key: string) => { label: string; hint: string }): SpendRow[] {
  return Object.entries(map)
    .map(([key, micros]) => ({ key, micros, ...describe(key), percent: total > 0 ? Math.max(1, Math.round((micros / total) * 100)) : 0 }))
    .sort((a, b) => b.micros - a.micros || a.key.localeCompare(b.key));
}

/** The three tables, largest first. A root shows its title when the log has the item, else its id. */
export function tables(s: Spend, items: readonly ItemRow[]): SpendTables {
  const titles = new Map(items.map((i) => [i.id, i.title]));
  const total = s.total_micros;
  return {
    total,
    roots: rows(s.by_root, total, (id) => ({ label: titles.get(id) ?? id, hint: titles.has(id) ? id : '' })),
    tiers: rows(s.by_review_tier, total, (tier) => ({ label: tier, hint: '' })),
    repos: rows(s.by_repo, total, (key) => {
      const at = key.indexOf(':');
      return at < 0 ? { label: key, hint: '' } : { label: key.slice(at + 1), hint: key.slice(0, at) };
    }),
  };
}
