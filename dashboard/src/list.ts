// Groups the fold's items for the list view. The stage itself comes from `core`
// through the fold; this only orders and labels what the fold decided.
import type { ItemRow } from './fold.ts';
import { inboxFor, oldestFirst } from './inbox.ts';
import { score } from './palette.ts';
import type { ViewState } from './state.ts';
import { applyFilters } from './views.ts';

/** Board order: what needs a person first, finished work last. Mirrors `Stage` in core/src/work_fold.rs. */
export const STAGES = [
  ['needs_you', 'Needs you'],
  ['triage', 'Triage'],
  ['proposed', 'Proposed'],
  ['in_review', 'In review'],
  ['ready', 'Ready'],
  ['blocked', 'Blocked'],
  ['held', 'Held'],
  ['done', 'Done'],
  ['closed', 'Closed'],
] as const;

export interface Group {
  stage: string;
  label: string;
  items: ItemRow[];
}

/** A stage the page does not know yet still shows, under its own name, after the known ones. */
export function stageLabel(stage: string): string {
  const known = STAGES.find(([s]) => s === stage);
  if (known) return known[1];
  const words = stage.replaceAll('_', ' ');
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** Items matching `query` by id or title, grouped by stage in board order, empty groups left out. */
export function group(items: readonly ItemRow[], query = ''): Group[] {
  const matching = query.trim() ? items.filter((i) => score(i.title, query) >= 0 || score(i.id, query) >= 0) : items;
  const order = (s: string) => {
    const at = STAGES.findIndex(([k]) => k === s);
    return at < 0 ? STAGES.length : at;
  };
  const byStage = new Map<string, ItemRow[]>();
  for (const item of matching) byStage.set(item.stage, [...(byStage.get(item.stage) ?? []), item]);
  return [...byStage.entries()]
    .sort(([a], [b]) => order(a) - order(b) || a.localeCompare(b))
    .map(([stage, rows]) => ({ stage, label: stageLabel(stage), items: rows }));
}

/** The rows in display order, which is the order j and k walk. */
export function flatten(groups: readonly Group[]): ItemRow[] {
  return groups.flatMap((g) => g.items);
}

/** The id `step` rows away from `current`, clamped to the ends. No current selection starts at the first row. */
export function move(rows: readonly ItemRow[], current: string | null, step: number): string | null {
  if (rows.length === 0) return null;
  const at = rows.findIndex((r) => r.id === current);
  if (at < 0) return rows[step < 0 ? rows.length - 1 : 0].id;
  return rows[Math.max(0, Math.min(rows.length - 1, at + step))].id;
}

/** What waits on `viewer`, in the fold's order: see `inboxFor`. */
export const myQueue = (items: readonly ItemRow[], viewer: string | null): ItemRow[] => inboxFor(items, viewer);

/** The items the view covers, before filters and search: every item, or only the Blocked ones. */
export function inScope(s: Pick<ViewState, 'items' | 'blocked' | 'view'> & { tracker?: ViewState['tracker']; kind?: ViewState['kind'] }): ItemRow[] {
  if (s.view === 'all') return s.items;
  if (s.view === 'inbox') return inboxFor(s.items, s.tracker?.kind === 'signed-in' ? s.tracker.person : null, s.kind ?? null);
  const blocked = new Set(s.blocked.map((b) => b.id));
  return s.items.filter((i) => blocked.has(i.id));
}

/** What the page shows: the items in the view, filtered, matching the search, grouped by stage. */
export function visible(s: Pick<ViewState, 'items' | 'blocked' | 'view' | 'query' | 'filters'> & { tracker?: ViewState['tracker']; kind?: ViewState['kind']; waiting?: ViewState['waiting'] }): Group[] {
  const rows = applyFilters(inScope(s), s.filters);
  if (s.view !== 'inbox') return group(rows, s.query);
  // One list, oldest first: the inbox is cleared from the top, not by stage.
  const matching = group(rows, s.query).flatMap((g) => g.items);
  return matching.length ? [{ stage: 'inbox', label: 'Waiting on you', items: oldestFirst(matching, s.waiting ?? new Map()) }] : [];
}

/** The rows the list shows, in the order j and k walk. */
export function shownRows(s: Pick<ViewState, 'items' | 'blocked' | 'view' | 'query' | 'filters'> & { tracker?: ViewState['tracker']; kind?: ViewState['kind']; waiting?: ViewState['waiting'] }): ItemRow[] {
  return flatten(visible(s));
}

/** The marked items the view shows now. A bulk action never reaches an item a search or filter hides. */
export function markedShown(s: Pick<ViewState, 'items' | 'blocked' | 'view' | 'query' | 'filters' | 'marked'> & { tracker?: ViewState['tracker']; kind?: ViewState['kind']; waiting?: ViewState['waiting'] }): ItemRow[] {
  const marked = new Set(s.marked);
  return shownRows(s).filter((i) => marked.has(i.id));
}

export type Direction = 'up' | 'down' | 'left' | 'right';

/**
 * The id one step from `current` on the board, where each group is a column. Up and
 * down stay in the column; left and right keep the row, or the column's last card
 * when it is shorter. The edges stop. No current card starts at the first one.
 */
export function moveBoard(columns: readonly Group[], current: string | null, dir: Direction): string | null {
  const col = columns.findIndex((g) => g.items.some((i) => i.id === current));
  if (col < 0) return columns[0]?.items[0]?.id ?? null;
  const row = columns[col].items.findIndex((i) => i.id === current);
  if (dir === 'up' || dir === 'down') {
    const items = columns[col].items;
    return items[Math.max(0, Math.min(items.length - 1, row + (dir === 'down' ? 1 : -1)))].id;
  }
  const next = columns[Math.max(0, Math.min(columns.length - 1, col + (dir === 'right' ? 1 : -1)))];
  return next.items[Math.min(row, next.items.length - 1)].id;
}

/** The first or last card of `current`'s column, so Home and End stay in the column. No current card uses the first column. */
export function boardEdge(columns: readonly Group[], current: string | null, edge: 'first' | 'last'): string | null {
  const items = (columns.find((g) => g.items.some((i) => i.id === current)) ?? columns[0])?.items ?? [];
  return (edge === 'first' ? items[0] : items.at(-1))?.id ?? null;
}

/**
 * Where the focus goes when `gone` leaves the page: the nearest id after it in `before`
 * that is still `shown`, else the nearest before it. Undefined when none is left.
 */
export function neighbour(before: readonly string[], gone: string, shown: (id: string) => boolean): string | undefined {
  const at = before.indexOf(gone);
  if (at < 0) return undefined;
  return [...before.slice(at + 1), ...before.slice(0, at).reverse()].find(shown);
}
