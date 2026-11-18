// Groups the fold's items for the list view. The stage itself comes from `core`
// through the fold; this only orders and labels what the fold decided.
import type { ItemRow } from './fold.ts';
import { inboxFor, oldestFirst } from './inbox.ts';
import { score } from './palette.ts';
import type { ViewState } from './state.ts';
import { applyFilters } from './views.ts';
import { STAGES, stageLabel } from './stages.ts';
import { collapseKey, effectiveGroup, groupItems, NO_GROUP } from './group-by.ts';

export { STAGES, stageLabel } from './stages.ts';

export interface Group {
  stage: string;
  label: string;
  items: ItemRow[];
  /** Set on a group the list can fold away: the key its collapsed state is kept under. */
  toggle?: string;
  /** Folded away: the header and count show, its rows do not, and j and k walk past them. */
  collapsed?: boolean;
  /** Set on a board swimlane's cell: the stage id of its column. The cell's `label` names the lane too. */
  column?: string;
}

/** One board row: the items sharing a Group by value, split into the board's stage columns. */
export interface Lane {
  value: string;
  label: string;
  /** The key the lane's collapsed state is kept under. */
  toggle: string;
  collapsed: boolean;
  /** One cell per board column, empty ones included so the columns line up from lane to lane. */
  cells: Group[];
}

/** A root's stage as the panel and cards name it: a ready root is ready to fix, once triage is accepted. */
export function rootStageLabel(item: Pick<ItemRow, 'stage' | 'root'>): string {
  return item.root && item.stage === 'ready' ? 'Ready to fix' : stageLabel(item.stage);
}

/** The small chip text for a root's triage child, or null when it has none. */
export function triageChip(item: Pick<ItemRow, 'triage' | 'triage_skipped' | 'root'>): string | null {
  if (item.root && item.triage_skipped) return 'Triage skipped';
  return item.root && item.triage ? `Triage: ${stageLabel(item.triage).toLowerCase()}` : null;
}

/** Why a root has no triage child, or null: the rule that admitted it past triage. */
export function triageSkipReason(item: Pick<ItemRow, 'triage_skipped' | 'root'>): string | null {
  return item.root && item.triage_skipped ? `triage skipped by rule ${item.triage_skipped}` : null;
}

/** Items matching `query` by id or title, grouped by stage in board order, empty groups left out. */
export function group(items: readonly ItemRow[], query = '', hide: (i: ItemRow) => boolean = () => false): Group[] {
  const matching = (query.trim() ? items.filter((i) => matches(i, query)) : [...items]).filter((i) => !hide(i));
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

/** Whether the search matches the item's id, title or upstream reference. */
export function matches(i: ItemRow, query: string): boolean {
  return score(i.title, query) >= 0 || score(i.id, query) >= 0 || (i.upstream ? score(i.upstream, query) >= 0 : false);
}

/**
 * Child work shown under its root instead of as a card: an item whose parent is also
 * in `shown`. A child that needs a person stays a card, so nothing waiting on someone hides.
 */
export function nestedIds(shown: readonly ItemRow[]): Set<string> {
  const ids = new Set(shown.map((i) => i.id));
  return new Set(shown.filter((i) => i.parent && ids.has(i.parent) && i.stage !== 'needs_you').map((i) => i.id));
}

/** The rows in display order, which is the order j and k walk. A collapsed group has no rows on the page. */
export function flatten(groups: readonly Group[]): ItemRow[] {
  return groups.flatMap((g) => (g.collapsed ? [] : g.items));
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

type Shown = Pick<ViewState, 'items' | 'blocked' | 'view' | 'query' | 'filters'> & { tracker?: ViewState['tracker']; kind?: ViewState['kind']; waiting?: ViewState['waiting']; childCards?: boolean; layout?: ViewState['layout']; group?: ViewState['group']; collapsed?: ViewState['collapsed'] };

/**
 * What the page shows: the items in the view, filtered, matching the search, grouped by
 * the view's Group by (stage unless a person chose otherwise). The board's columns are
 * stages; grouped by anything else it is the cells of each swimlane, lane after lane. A
 * collapsed group stays, with its items, marked `collapsed`.
 */
export function visible(s: Shown): Group[] {
  const { shown, by, board } = placed(s);
  if (s.view === 'inbox' && by === NO_GROUP) return shown.length ? [{ stage: 'inbox', label: 'Waiting on you', items: shown }] : [];
  if (by === NO_GROUP && !board) return shown.length ? [{ stage: NO_GROUP, label: '', items: shown }] : [];
  if (board) return swimlanes(s)?.flatMap((l) => l.cells) ?? group(shown);
  const folded = new Set(s.collapsed ?? []);
  return groupItems(shown, by).map((g) => {
    const toggle = collapseKey(s.view, by, g.value);
    return { stage: g.value === '' ? `no-${by}` : g.value, label: g.label, items: g.items, toggle, collapsed: folded.has(toggle) };
  });
}

/** The items the page shows, in order, with the grouping that applies to them. */
function placed(s: Shown) {
  const rows = applyFilters(inScope(s), s.filters);
  const board = s.layout === 'board' && s.view !== 'inbox';
  const by = effectiveGroup(s.view, s.group);
  const found = s.query.trim() ? rows.filter((i) => matches(i, s.query)) : rows;
  // The inbox is cleared from the top, oldest first, whatever it is grouped by.
  const ordered = s.view === 'inbox' ? oldestFirst(found, s.waiting ?? new Map()) : found;
  const nested = s.childCards || s.view === 'inbox' ? new Set<string>() : nestedIds(found);
  return { shown: ordered.filter((i) => !nested.has(i.id)), by, board };
}

/**
 * The board's rows when it is grouped by something other than stage: one lane per value,
 * each split into the same stage columns. Null when the columns stand alone.
 */
export function swimlanes(s: Shown): Lane[] | null {
  const { shown, by, board } = placed(s);
  if (!board || by === NO_GROUP || by === 'stage') return null;
  const columns = group(shown);
  const folded = new Set(s.collapsed ?? []);
  return groupItems(shown, by).map((g) => {
    const toggle = collapseKey(s.view, by, g.value);
    const collapsed = folded.has(toggle);
    const lane = g.value === '' ? `no-${by}` : g.value;
    // The lane's toggle rides on its first cell, so folding every group counts each lane once.
    const cells = columns.map((c, at): Group => ({ stage: `${lane}|${c.stage}`, column: c.stage, label: `${c.label}, ${g.label}`, items: g.items.filter((i) => i.stage === c.stage), collapsed, toggle: at === 0 ? toggle : undefined }));
    return { value: lane, label: g.label, toggle, collapsed, cells };
  });
}

/** The rows the list shows, in the order j and k walk. */
export function shownRows(s: Shown): ItemRow[] {
  return flatten(visible(s));
}

/**
 * What the list selection should be after `id` is opened. A nested child has no row of
 * its own, so the selection goes to the nearest root that does; an item the view does not
 * show at all keeps the selection on itself, as before.
 */
export function selectionFor(s: Shown, id: string): string {
  const shown = new Set(shownRows(s).map((r) => r.id));
  const byId = new Map(s.items.map((i) => [i.id, i]));
  const seen = new Set<string>();
  for (let at = byId.get(id); at && !seen.has(at.id); at = at.parent ? byId.get(at.parent) : undefined) {
    if (shown.has(at.id)) return at.id;
    seen.add(at.id);
  }
  return id;
}

/** The marked items the view shows now. A bulk action never reaches an item a search, a filter or a collapsed group hides. */
export function markedShown(s: Shown & Pick<ViewState, 'marked'>): ItemRow[] {
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

/**
 * The selection after the groups change: a selected row that sits in a collapsed group has
 * no row on the page, so the selection moves to the nearest row that does, else none. A
 * selection the view never listed (an opened link) is left alone.
 */
export function settleSelection(s: Shown & { selected: string | null }): string | null {
  if (!s.selected) return s.selected;
  const groups = visible(s);
  const all = groups.flatMap((g) => g.items.map((i) => i.id));
  if (!all.includes(s.selected)) return s.selected;
  const shown = new Set(flatten(groups).map((i) => i.id));
  return shown.has(s.selected) ? s.selected : (neighbour(all, s.selected, (id) => shown.has(id)) ?? null);
}

/** The cards of one board column across the open lanes, top lane first. */
const columnCards = (lanes: readonly Lane[], column: string): string[] =>
  lanes.filter((l) => !l.collapsed).flatMap((l) => l.cells.filter((c) => c.column === column).flatMap((c) => c.items.map((i) => i.id)));

/**
 * `moveBoard` for swimlanes. Up and down walk the column through every open lane, so the
 * last card of a cell steps into the next lane's cell; left and right stay in the lane and
 * skip columns it has no card in. A collapsed lane has no cards to land on. The edges stop.
 */
export function moveLanes(lanes: readonly Lane[], current: string | null, dir: Direction): string | null {
  const open = lanes.filter((l) => !l.collapsed);
  const first = open.flatMap((l) => l.cells).find((c) => c.items.length);
  const lane = open.find((l) => l.cells.some((c) => c.items.some((i) => i.id === current)));
  if (!lane) return first?.items[0].id ?? null;
  const at = lane.cells.findIndex((c) => c.items.some((i) => i.id === current));
  const cell = lane.cells[at];
  if (dir === 'up' || dir === 'down') {
    const cards = columnCards(lanes, cell.column!);
    return cards[Math.max(0, Math.min(cards.length - 1, cards.indexOf(current!) + (dir === 'down' ? 1 : -1)))];
  }
  const row = cell.items.findIndex((i) => i.id === current);
  const step = dir === 'right' ? 1 : -1;
  for (let k = at + step; k >= 0 && k < lane.cells.length; k += step) {
    const next = lane.cells[k].items;
    if (next.length) return next[Math.min(row, next.length - 1)].id;
  }
  return current;
}

/** The first or last card of `current`'s column across the open lanes; no current card uses the first column that has cards. */
export function laneEdge(lanes: readonly Lane[], current: string | null, edge: 'first' | 'last'): string | null {
  const open = lanes.filter((l) => !l.collapsed);
  const column = open.flatMap((l) => l.cells).find((c) => c.items.some((i) => i.id === current))?.column ?? open.flatMap((l) => l.cells).find((c) => c.items.length)?.column;
  if (!column) return null;
  const cards = columnCards(lanes, column);
  return (edge === 'first' ? cards[0] : cards.at(-1)) ?? null;
}
