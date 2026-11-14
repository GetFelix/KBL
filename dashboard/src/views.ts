// Filters, the view in the address, and saved views. Filters only offer values the
// log recorded, so a facet nobody recorded has nothing to pick and is not shown.
import type { ItemRow } from './fold.ts';

export const FACETS = [
  ['source', 'Source'],
  ['tier', 'Tier'],
  ['repo', 'Repo'],
] as const;

export type Facet = (typeof FACETS)[number][0];

/** One value per facet; a missing facet matches everything. */
export type Filters = Partial<Record<Facet, string>>;

export type Layout = 'list' | 'board';

/** Everything a link or a saved view restores. */
export interface ViewSpec {
  layout: Layout;
  query: string;
  filters: Filters;
}

export interface SavedView {
  name: string;
  /** The view as a query string, from `encodeView`. */
  search: string;
}

/** An item's recorded value for a facet, as text, or null when nothing was recorded. */
export function valueOf(item: ItemRow, facet: Facet): string | null {
  const v = item[facet];
  return v === null || v === undefined || v === '' ? null : String(v);
}

/** The recorded values of each facet with how many items have them, sorted. Facets with none are left out. */
export function facets(items: readonly ItemRow[]): { facet: Facet; label: string; values: [string, number][] }[] {
  return FACETS.flatMap(([facet, label]) => {
    const counts = new Map<string, number>();
    for (const i of items) {
      const v = valueOf(i, facet);
      if (v !== null) counts.set(v, (counts.get(v) ?? 0) + 1);
    }
    const values = [...counts.entries()].sort(([a], [b]) => a.localeCompare(b, undefined, { numeric: true }));
    return values.length ? [{ facet, label, values }] : [];
  });
}

export function applyFilters(items: readonly ItemRow[], f: Filters): ItemRow[] {
  const active = FACETS.filter(([k]) => f[k] !== undefined);
  if (active.length === 0) return [...items];
  return items.filter((i) => active.every(([k]) => valueOf(i, k) === f[k]));
}

export function activeCount(f: Filters): number {
  return FACETS.filter(([k]) => f[k] !== undefined).length;
}

/** The active filters in words, such as `Source github, Tier 1`, or empty. */
export function summary(f: Filters): string {
  return FACETS.filter(([k]) => f[k] !== undefined)
    .map(([k, label]) => `${label} ${f[k]}`)
    .join(', ');
}

export function setFilter(f: Filters, facet: Facet, value: string | null): Filters {
  const next = { ...f };
  if (value === null) delete next[facet];
  else next[facet] = value;
  return next;
}

const PARAMS = { layout: 'view', query: 'q' } as const;

/** Reads the view from a query string. Unknown layouts read as the list. */
export function decodeView(search: string): ViewSpec {
  const q = new URLSearchParams(search);
  const filters: Filters = {};
  for (const [k] of FACETS) {
    const v = q.get(k);
    if (v) filters[k] = v;
  }
  return { layout: q.get(PARAMS.layout) === 'board' ? 'board' : 'list', query: q.get(PARAMS.query) ?? '', filters };
}

/** Writes the view into `search`, keeping every other parameter. Defaults are left out, so a plain view has a plain address. */
export function encodeView(search: string, v: ViewSpec): string {
  const q = new URLSearchParams(search);
  const set = (k: string, value: string | undefined) => (value ? q.set(k, value) : q.delete(k));
  set(PARAMS.layout, v.layout === 'board' ? 'board' : undefined);
  set(PARAMS.query, v.query);
  for (const [k] of FACETS) set(k, v.filters[k]);
  return q.toString();
}

/** True when two views show the same thing, whatever order their parameters are in. */
export function sameView(a: ViewSpec, b: ViewSpec): boolean {
  return encodeView('', a) === encodeView('', b);
}

/** Saved views from storage. Anything malformed is dropped rather than breaking the page. */
export function parseSaved(json: string | null): SavedView[] {
  if (!json) return [];
  try {
    const raw: unknown = JSON.parse(json);
    if (!Array.isArray(raw)) return [];
    return raw.filter(
      (v): v is SavedView => typeof v === 'object' && v !== null && typeof v.name === 'string' && v.name.trim() !== '' && typeof v.search === 'string',
    );
  } catch {
    return [];
  }
}

/** Saves `view` as `name`. A view with the same name (ignoring case) is replaced in place. */
export function saveView(list: readonly SavedView[], name: string, view: ViewSpec): SavedView[] {
  const entry = { name: name.trim(), search: encodeView('', view) };
  const at = list.findIndex((v) => v.name.toLowerCase() === entry.name.toLowerCase());
  return at < 0 ? [...list, entry] : list.map((v, i) => (i === at ? entry : v));
}

/** Renames a view. A name another view already has is refused (returns null), so nothing is lost silently. */
export function renameView(list: readonly SavedView[], from: string, to: string): SavedView[] | null {
  const name = to.trim();
  if (!name) return null;
  if (list.some((v) => v.name !== from && v.name.toLowerCase() === name.toLowerCase())) return null;
  return list.map((v) => (v.name === from ? { ...v, name } : v));
}

export function deleteView(list: readonly SavedView[], name: string): SavedView[] {
  return list.filter((v) => v.name !== name);
}
