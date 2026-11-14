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

/** Which items are shown: all of them, or only the ones a link holds back. */
export type View = 'all' | 'blocked';

/** Everything a link or a saved view restores. */
export interface ViewSpec {
  view: View;
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

/**
 * The recorded values of each facet, sorted, with how many items have them under the
 * other active filters (so a count is what picking that value would show). Every
 * recorded value is offered, even at 0. Facets nobody recorded are left out.
 */
export function facets(items: readonly ItemRow[], filters: Filters = {}): { facet: Facet; label: string; values: [string, number][] }[] {
  return FACETS.flatMap(([facet, label]) => {
    const counts = new Map<string, number>();
    for (const i of items) {
      const v = valueOf(i, facet);
      if (v !== null && !counts.has(v)) counts.set(v, 0);
    }
    for (const i of applyFilters(items, setFilter(filters, facet, null))) {
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

const PARAMS = { view: 'view', layout: 'layout', query: 'q' } as const;

/**
 * Reads the view from a query string. Unknown values read as all work in a list. The board
 * was once `view=board`; links and saved views from then still open it, and the next write
 * moves it to `layout`.
 */
export function decodeView(search: string): ViewSpec {
  const q = new URLSearchParams(search);
  const filters: Filters = {};
  for (const [k] of FACETS) {
    const v = q.get(k);
    if (v) filters[k] = v;
  }
  const view = q.get(PARAMS.view);
  const board = q.get(PARAMS.layout) === 'board' || view === 'board';
  return { view: view === 'blocked' ? 'blocked' : 'all', layout: board ? 'board' : 'list', query: q.get(PARAMS.query) ?? '', filters };
}

/** Writes the view into `search`, keeping every other parameter. Defaults are left out, so a plain view has a plain address. */
export function encodeView(search: string, v: ViewSpec): string {
  const q = new URLSearchParams(search);
  const set = (k: string, value: string | undefined) => (value ? q.set(k, value) : q.delete(k));
  set(PARAMS.view, v.view === 'blocked' ? 'blocked' : undefined);
  set(PARAMS.layout, v.layout === 'board' ? 'board' : undefined);
  set(PARAMS.query, v.query);
  for (const [k] of FACETS) set(k, v.filters[k]);
  return q.toString();
}

/** True when two views show the same thing, whatever order their parameters are in. */
export function sameView(a: ViewSpec, b: ViewSpec): boolean {
  return encodeView('', a) === encodeView('', b);
}

/** Saved views from storage, each rewritten to the current address form. Anything malformed is dropped rather than breaking the page. */
export function parseSaved(json: string | null): SavedView[] {
  if (!json) return [];
  try {
    const raw: unknown = JSON.parse(json);
    if (!Array.isArray(raw)) return [];
    return raw.filter(
      (v): v is SavedView => typeof v === 'object' && v !== null && typeof v.name === 'string' && v.name.trim() !== '' && typeof v.search === 'string',
    ).map((v) => ({ name: v.name, search: encodeView('', decodeView(v.search)) }));
  } catch {
    return [];
  }
}

/** Why `name` cannot name a view in `list`, or null. `editing` is the view being renamed, which may keep its own name. */
export function nameProblem(list: readonly SavedView[], name: string, editing: string | null = null): string | null {
  const n = name.trim();
  if (!n) return 'Give the view a name.';
  if (list.some((v) => v.name !== editing && v.name.toLowerCase() === n.toLowerCase())) {
    return `A view is already called “${n}”. Pick another name, or edit that view.`;
  }
  return null;
}

/** Adds `view` as `name`, or returns why not. A name another view has (ignoring case) is refused, so nothing is lost silently. */
export function saveView(list: readonly SavedView[], name: string, view: ViewSpec): SavedView[] | string {
  return nameProblem(list, name) ?? [...list, { name: name.trim(), search: encodeView('', view) }];
}

/** Renames a view, or returns why not: an empty or taken name, or a view another tab deleted. */
export function renameView(list: readonly SavedView[], from: string, to: string): SavedView[] | string {
  if (!list.some((v) => v.name === from)) return `“${from}” was deleted, perhaps in another tab.`;
  const problem = nameProblem(list, to, from);
  if (problem) return problem;
  return list.map((v) => (v.name === from ? { ...v, name: to.trim() } : v));
}

export function deleteView(list: readonly SavedView[], name: string): SavedView[] {
  return list.filter((v) => v.name !== name);
}
